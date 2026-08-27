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

// errIdleTimeout marks an SSE connection we abandoned because no data — not even
// the server's periodic keepalive — arrived for [idleTimeout]. The connection
// has silently died (laptop sleep, network change, NAT timeout) and must be
// reconnected.
var errIdleTimeout = errors.New("no data from server within idle timeout")

func (d *daemon) streamOnce(parent context.Context) error {
	// The SSE read blocks indefinitely, so a silently-dropped TCP connection
	// would otherwise wedge it forever with no error and no reconnect. The server
	// sends a keepalive comment every 30s; if we see no data at all for
	// idleTimeout, treat the connection as dead and cancel so we reconnect.
	const idleTimeout = 75 * time.Second

	ctx, cancel := context.WithCancelCause(parent)
	defer cancel(nil)

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

	// Idle watchdog: reset on every received line (events, data, or keepalives);
	// if the stream goes quiet past idleTimeout, cancel to unblock the read.
	activity := make(chan struct{}, 1)
	go func() {
		timer := time.NewTimer(idleTimeout)
		defer timer.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-activity:
				if !timer.Stop() {
					select {
					case <-timer.C:
					default:
					}
				}
				timer.Reset(idleTimeout)
			case <-timer.C:
				cancel(errIdleTimeout)
				return
			}
		}
	}()

	var eventName string
	var dataLines []string

	scanner := bufio.NewScanner(resp.Body)
	scanner.Buffer(make([]byte, 64*1024), 16*1024*1024)
	for scanner.Scan() {
		select {
		case activity <- struct{}{}:
		default:
		}
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
	if context.Cause(ctx) == errIdleTimeout {
		return errIdleTimeout
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

func run(ctx context.Context, args []string, stdout io.Writer) error {
	if len(args) > 0 && args[0] == "health" {
		flags := flag.NewFlagSet("tidepool-clipd health", flag.ContinueOnError)
		flags.SetOutput(io.Discard)
		timeout := flags.Duration("timeout", time.Second, "health query timeout")
		if err := flags.Parse(args[1:]); err != nil {
			return err
		}
		if flags.NArg() != 0 {
			return errors.New("health accepts no positional arguments")
		}
		if *timeout <= 0 {
			return errors.New("health timeout must be positive")
		}
		path, err := defaultHealthSocket()
		if err != nil {
			return err
		}
		queryCtx, cancel := healthContext(ctx, *timeout)
		defer cancel()
		status, err := queryHealth(queryCtx, path)
		if err != nil {
			return err
		}
		return json.NewEncoder(stdout).Encode(status)
	}

	flags := flag.NewFlagSet("tidepool-clipd", flag.ContinueOnError)
	flags.SetOutput(io.Discard)
	urlFlag := flags.String("url", "", "tidepool base URL, e.g. https://tidepool.tailnet.ts.net")
	pollFlag := flags.Duration("poll", 500*time.Millisecond, "local clipboard poll interval")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if flags.NArg() != 0 {
		return errors.New("unexpected positional arguments")
	}
	if *urlFlag == "" {
		return errors.New("must specify -url")
	}
	if *pollFlag <= 0 {
		return errors.New("poll interval must be positive")
	}

	cb, err := newClipboard()
	if err != nil {
		return fmt.Errorf("clipboard: %w", err)
	}
	executable, err := os.Executable()
	if err != nil {
		return fmt.Errorf("locate running binary: %w", err)
	}
	status, err := newHealthStatus(executable)
	if err != nil {
		return fmt.Errorf("initialize health status: %w", err)
	}
	healthSocket, err := defaultHealthSocket()
	if err != nil {
		return err
	}
	server, err := startHealthServer(healthSocket, status)
	if err != nil {
		return fmt.Errorf("start health server: %w", err)
	}
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()

	d := &daemon{
		baseURL: strings.TrimRight(*urlFlag, "/"),
		clip:    cb,
		poll:    *pollFlag,
		// No global timeout: the SSE stream is long-lived. Per-request
		// timeouts are enforced via context where needed.
		http: &http.Client{},
	}

	log.Printf("tidepool-clipd -> %s (poll %s, os %s)", d.baseURL, d.poll, runtime.GOOS)

	var wg sync.WaitGroup
	wg.Add(2)
	go func() { defer wg.Done(); d.watch(ctx) }()
	go func() { defer wg.Done(); d.subscribe(ctx) }()
	healthDone := make(chan error, 1)
	go func() { healthDone <- server.serve(ctx) }()

	var healthErr error
	healthFinished := false
	select {
	case <-ctx.Done():
	case healthErr = <-healthDone:
		healthFinished = true
	}
	cancel()
	wg.Wait()
	if !healthFinished {
		healthErr = <-healthDone
	}
	if healthErr != nil {
		return healthErr
	}
	log.Print("shutdown")
	return nil
}

func main() {
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	if err := run(ctx, os.Args[1:], os.Stdout); err != nil {
		log.Fatal(err)
	}
}
