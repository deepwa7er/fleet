package main

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"time"
)

const healthProtocol = 1

type healthStatus struct {
	Protocol     int    `json:"protocol"`
	PID          int    `json:"pid"`
	Instance     string `json:"instance"`
	BinarySHA256 string `json:"binary_sha256"`
	Ready        bool   `json:"ready"`
}

func newHealthStatus(executable string) (healthStatus, error) {
	binary, err := os.ReadFile(executable)
	if err != nil {
		return healthStatus{}, fmt.Errorf("read running binary: %w", err)
	}
	sum := sha256.Sum256(binary)

	instanceBytes := make([]byte, 16)
	if _, err := rand.Read(instanceBytes); err != nil {
		return healthStatus{}, fmt.Errorf("create process instance token: %w", err)
	}

	return healthStatus{
		Protocol:     healthProtocol,
		PID:          os.Getpid(),
		Instance:     hex.EncodeToString(instanceBytes),
		BinarySHA256: hex.EncodeToString(sum[:]),
		Ready:        true,
	}, nil
}

func defaultHealthSocket() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("locate user home directory: %w", err)
	}
	var cacheDir string
	switch runtime.GOOS {
	case "darwin":
		cacheDir = filepath.Join(home, "Library", "Caches")
	case "linux":
		cacheDir = filepath.Join(home, ".cache")
	default:
		return "", fmt.Errorf("unsupported OS for health socket: %s", runtime.GOOS)
	}
	return filepath.Join(cacheDir, "tidepool", "clipd-health.sock"), nil
}

type healthServer struct {
	listener net.Listener
	path     string
	status   healthStatus
	socket   os.FileInfo
}

func startHealthServer(path string, status healthStatus) (*healthServer, error) {
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, fmt.Errorf("create health socket directory: %w", err)
	}
	if err := os.Chmod(dir, 0o700); err != nil {
		return nil, fmt.Errorf("secure health socket directory: %w", err)
	}

	if info, err := os.Lstat(path); err == nil {
		if info.Mode()&os.ModeSocket == 0 {
			return nil, fmt.Errorf("health socket path exists and is not a socket: %s", path)
		}
		conn, dialErr := net.DialTimeout("unix", path, 100*time.Millisecond)
		if dialErr == nil {
			conn.Close()
			return nil, fmt.Errorf("another tidepool-clipd is serving health at %s", path)
		}
		if err := os.Remove(path); err != nil {
			return nil, fmt.Errorf("remove stale health socket: %w", err)
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return nil, fmt.Errorf("inspect health socket: %w", err)
	}

	listener, err := net.Listen("unix", path)
	if err != nil {
		return nil, fmt.Errorf("listen on health socket: %w", err)
	}
	if err := os.Chmod(path, 0o600); err != nil {
		listener.Close()
		os.Remove(path)
		return nil, fmt.Errorf("secure health socket: %w", err)
	}
	socket, err := os.Lstat(path)
	if err != nil {
		listener.Close()
		os.Remove(path)
		return nil, fmt.Errorf("inspect new health socket: %w", err)
	}

	return &healthServer{listener: listener, path: path, status: status, socket: socket}, nil
}

func (s *healthServer) serve(ctx context.Context) error {
	closed := make(chan struct{})
	go func() {
		select {
		case <-ctx.Done():
			s.listener.Close()
		case <-closed:
		}
	}()
	defer close(closed)
	defer s.listener.Close()
	defer s.removeOwnedSocket()

	for {
		conn, err := s.listener.Accept()
		if err != nil {
			if ctx.Err() != nil || errors.Is(err, net.ErrClosed) {
				return nil
			}
			return fmt.Errorf("accept health connection: %w", err)
		}
		if err := json.NewEncoder(conn).Encode(s.status); err != nil {
			conn.Close()
			continue
		}
		conn.Close()
	}
}

func (s *healthServer) removeOwnedSocket() {
	current, err := os.Lstat(s.path)
	if err == nil && os.SameFile(current, s.socket) {
		os.Remove(s.path)
	}
}

func queryHealth(ctx context.Context, path string) (healthStatus, error) {
	conn, err := (&net.Dialer{}).DialContext(ctx, "unix", path)
	if err != nil {
		return healthStatus{}, fmt.Errorf("connect to health socket: %w", err)
	}
	defer conn.Close()
	if deadline, ok := ctx.Deadline(); ok {
		if err := conn.SetDeadline(deadline); err != nil {
			return healthStatus{}, fmt.Errorf("set health deadline: %w", err)
		}
	}

	var status healthStatus
	decoder := json.NewDecoder(io.LimitReader(conn, 4097))
	if err := decoder.Decode(&status); err != nil {
		return healthStatus{}, fmt.Errorf("decode health response: %w", err)
	}
	if status.Protocol != healthProtocol {
		return healthStatus{}, fmt.Errorf("unsupported health protocol %d", status.Protocol)
	}
	if status.PID <= 0 {
		return healthStatus{}, errors.New("health response has invalid PID")
	}
	if len(status.Instance) != 32 {
		return healthStatus{}, errors.New("health response has invalid instance token")
	}
	if _, err := hex.DecodeString(status.Instance); err != nil {
		return healthStatus{}, errors.New("health response has invalid instance token")
	}
	if len(status.BinarySHA256) != sha256.Size*2 {
		return healthStatus{}, errors.New("health response has invalid binary SHA-256")
	}
	if _, err := hex.DecodeString(status.BinarySHA256); err != nil {
		return healthStatus{}, errors.New("health response has invalid binary SHA-256")
	}
	if !status.Ready {
		return healthStatus{}, errors.New("daemon reports that it is not ready")
	}
	return status, nil
}

func healthContext(parent context.Context, timeout time.Duration) (context.Context, context.CancelFunc) {
	return context.WithTimeout(parent, timeout)
}
