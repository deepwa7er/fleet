package main

import (
	"bufio"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"os/signal"
	"runtime"
	"strings"
	"sync"
	"syscall"
	"time"
)

type clipboard interface {
	Read() (string, error)
	Write(string) error
}

type macClipboard struct{}

func (macClipboard) Read() (string, error) {
	out, err := exec.Command("pbpaste").Output()
	return string(out), err
}

func (macClipboard) Write(s string) error {
	cmd := exec.Command("pbcopy")
	cmd.Stdin = strings.NewReader(s)
	return cmd.Run()
}

type waylandClipboard struct{}

func (waylandClipboard) Read() (string, error) {
	out, err := exec.Command("wl-paste", "--no-newline").Output()
	if err != nil {
		// wl-paste exits non-zero when the clipboard is empty or holds
		// non-text content. Treat that as empty string.
		var exit *exec.ExitError
		if errors.As(err, &exit) && len(out) == 0 {
			return "", nil
		}
		return "", err
	}
	return string(out), nil
}

func (waylandClipboard) Write(s string) error {
	cmd := exec.Command("wl-copy", "-n")
	cmd.Stdin = strings.NewReader(s)
	return cmd.Run()
}

func newClipboard() (clipboard, error) {
	switch runtime.GOOS {
	case "darwin":
		return macClipboard{}, nil
	case "linux":
		if os.Getenv("WAYLAND_DISPLAY") == "" {
			return nil, errors.New("WAYLAND_DISPLAY not set; only Wayland is supported")
		}
		return waylandClipboard{}, nil
	default:
		return nil, fmt.Errorf("unsupported OS: %s", runtime.GOOS)
	}
}

type daemon struct {
	baseURL string
	clip    clipboard
	poll    time.Duration
	http    *http.Client

	mu       sync.Mutex
	lastHash string
}

// hashAndSet returns whether the value differs from the last seen one,
// and atomically updates lastHash so concurrent paths cannot race the
// dedup check.
func (d *daemon) hashAndSet(s string) (changed bool) {
	sum := sha256.Sum256([]byte(s))
	h := hex.EncodeToString(sum[:])
	d.mu.Lock()
	defer d.mu.Unlock()
	if h == d.lastHash {
		return false
	}
	d.lastHash = h
	return true
}

func (d *daemon) watch(ctx context.Context) {
	t := time.NewTicker(d.poll)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			s, err := d.clip.Read()
			if err != nil {
				log.Printf("clip read: %v", err)
				continue
			}
			if !d.hashAndSet(s) {
				continue
			}
			if err := d.postClip(ctx, s); err != nil {
				log.Printf("post clip: %v", err)
			}
		}
	}
}

func (d *daemon) subscribe(ctx context.Context) {
	const retryDelay = 5 * time.Second
	for {
		if ctx.Err() != nil {
			return
		}
		if err := d.streamOnce(ctx); err != nil && ctx.Err() == nil {
			log.Printf("stream: %v (reconnecting in %s)", err, retryDelay)
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(retryDelay):
		}
	}
}

func (d *daemon) streamOnce(ctx context.Context) error {
	req, err := http.NewRequestWithContext(ctx, "GET", d.baseURL+"/clip/stream", nil)
	if err != nil {
		return err
	}
	req.Header.Set("Accept", "text/event-stream")

	resp, err := d.http.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("status %d", resp.StatusCode)
	}

	var eventName string
	var dataLines []string

	scanner := bufio.NewScanner(resp.Body)
	scanner.Buffer(make([]byte, 64*1024), 16*1024*1024)
	for scanner.Scan() {
		line := scanner.Text()
		switch {
		case line == "":
			if eventName == "clip" && len(dataLines) > 0 {
				d.onClipEvent(strings.Join(dataLines, "\n"))
			}
			eventName, dataLines = "", nil
		case strings.HasPrefix(line, ":"):
			// comment / keepalive
		default:
			k, v, ok := strings.Cut(line, ":")
			if !ok {
				continue
			}
			v = strings.TrimPrefix(v, " ")
			switch k {
			case "event":
				eventName = v
			case "data":
				dataLines = append(dataLines, v)
			}
		}
	}
	return scanner.Err()
}

func (d *daemon) onClipEvent(data string) {
	var ev struct {
		Text      string `json:"text"`
		UpdatedBy string `json:"updated_by"`
	}
	if err := json.Unmarshal([]byte(data), &ev); err != nil {
		log.Printf("clip event parse: %v", err)
		return
	}
	if !d.hashAndSet(ev.Text) {
		return
	}
	if err := d.clip.Write(ev.Text); err != nil {
		log.Printf("clip write: %v", err)
	}
}

func (d *daemon) postClip(ctx context.Context, text string) error {
	form := url.Values{"text": {text}}
	req, err := http.NewRequestWithContext(ctx, "POST", d.baseURL+"/clip", strings.NewReader(form.Encode()))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	resp, err := d.http.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("status %d", resp.StatusCode)
	}
	return nil
}

func main() {
	urlFlag := flag.String("url", "", "tidepool base URL, e.g. https://tidepool.tailnet.ts.net")
	pollFlag := flag.Duration("poll", 500*time.Millisecond, "local clipboard poll interval")
	flag.Parse()
	if *urlFlag == "" {
		log.Fatal("must specify -url")
	}

	cb, err := newClipboard()
	if err != nil {
		log.Fatalf("clipboard: %v", err)
	}

	d := &daemon{
		baseURL: strings.TrimRight(*urlFlag, "/"),
		clip:    cb,
		poll:    *pollFlag,
		// No global timeout: the SSE stream is long-lived. Per-request
		// timeouts are enforced via context where needed.
		http: &http.Client{},
	}

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()

	log.Printf("tidepool-clipd -> %s (poll %s, os %s)", d.baseURL, d.poll, runtime.GOOS)

	var wg sync.WaitGroup
	wg.Add(2)
	go func() { defer wg.Done(); d.watch(ctx) }()
	go func() { defer wg.Done(); d.subscribe(ctx) }()
	wg.Wait()
	log.Print("shutdown")
}
