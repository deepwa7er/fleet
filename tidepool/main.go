package main

import (
	"context"
	"errors"
	"flag"
	"log"
	"net"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	"tailscale.com/tsnet"

	"tidepool/internal/server"
	"tidepool/internal/store"
)

func main() {
	var (
		dev      = flag.Bool("dev", false, "run on localhost over plain HTTP (no tsnet)")
		addr     = flag.String("addr", ":8080", "dev-mode listen address")
		dataDir  = flag.String("data", "./data", "directory for blobs + sqlite db")
		tsState  = flag.String("tsnet-state", "./tsnet-state", "tsnet state directory")
		hostname = flag.String("hostname", "tidepool", "tailnet hostname")
		ttl      = flag.Duration("ttl", 24*time.Hour, "file retention")
		maxMB    = flag.Int64("max-mb", 100, "max upload size in MB")
		useTLS   = flag.Bool("tls", true, "use tsnet ListenTLS (requires HTTPS enabled on your tailnet)")
	)
	flag.Parse()

	if err := os.MkdirAll(filepath.Join(*dataDir, "blobs"), 0o755); err != nil {
		log.Fatalf("mkdir data: %v", err)
	}

	db, err := store.Open(filepath.Join(*dataDir, "tidepool.db"))
	if err != nil {
		log.Fatalf("open db: %v", err)
	}
	defer db.Close()

	cfg := server.Config{
		DB:        db,
		BlobDir:   filepath.Join(*dataDir, "blobs"),
		TTL:       *ttl,
		MaxUpload: *maxMB * 1024 * 1024,
	}

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()

	go server.RunSweeper(ctx, cfg)

	if *dev {
		ln, err := net.Listen("tcp", *addr)
		if err != nil {
			log.Fatalf("listen %s: %v", *addr, err)
		}
		log.Printf("tidepool dev: http://localhost%s", *addr)
		serve(ctx, ln, server.New(cfg))
		return
	}

	ts := &tsnet.Server{
		Dir:      *tsState,
		Hostname: *hostname,
		Logf:     func(format string, args ...any) { log.Printf("tsnet: "+format, args...) },
	}
	defer ts.Close()

	lc, err := ts.LocalClient()
	if err != nil {
		log.Fatalf("tsnet local client: %v", err)
	}
	cfg.WhoIs = func(ctx context.Context, remoteAddr string) string {
		who, err := lc.WhoIs(ctx, remoteAddr)
		if err != nil || who == nil {
			return ""
		}
		if who.Node != nil && who.Node.ComputedName != "" {
			return who.Node.ComputedName
		}
		if who.UserProfile != nil {
			return who.UserProfile.LoginName
		}
		return ""
	}

	var ln net.Listener
	if *useTLS {
		ln, err = ts.ListenTLS("tcp", ":443")
		if err != nil {
			log.Fatalf("tsnet ListenTLS :443: %v (is HTTPS enabled on your tailnet?)", err)
		}
		log.Printf("tidepool tsnet HTTPS on %s.<tailnet>.ts.net:443", *hostname)
	} else {
		ln, err = ts.Listen("tcp", ":80")
		if err != nil {
			log.Fatalf("tsnet Listen :80: %v", err)
		}
		log.Printf("tidepool tsnet HTTP on %s:80", *hostname)
	}
	serve(ctx, ln, server.New(cfg))
}

func serve(ctx context.Context, ln net.Listener, h http.Handler) {
	srv := &http.Server{
		Handler:           h,
		ReadHeaderTimeout: 15 * time.Second,
	}
	go func() {
		<-ctx.Done()
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		_ = srv.Shutdown(shutdownCtx)
	}()
	if err := srv.Serve(ln); err != nil && !errors.Is(err, http.ErrServerClosed) {
		log.Fatalf("serve: %v", err)
	}
}
