package server

import (
	"context"
	"crypto/rand"
	"embed"
	"encoding/base64"
	"errors"
	"fmt"
	"html/template"
	"io"
	"io/fs"
	"log"
	"net/http"
	"os"
	"path/filepath"
	"time"

	"github.com/go-chi/chi/v5"
	"github.com/go-chi/chi/v5/middleware"

	"tidepool/internal/store"
)

//go:embed all:templates
var templatesFS embed.FS

//go:embed all:static
var staticFS embed.FS

// Config carries server dependencies and tunables.
type Config struct {
	DB        *store.DB
	BlobDir   string
	TTL       time.Duration
	MaxUpload int64
	// WhoIs resolves a remote address (as r.RemoteAddr) to a device label.
	// May be nil in dev mode.
	WhoIs func(ctx context.Context, remoteAddr string) string
}

type server struct {
	cfg Config
	tpl *template.Template
}

// New builds the HTTP handler.
func New(cfg Config) http.Handler {
	tpl := template.Must(template.New("").Funcs(funcs).ParseFS(templatesFS, "templates/*.html"))
	s := &server{cfg: cfg, tpl: tpl}

	r := chi.NewRouter()
	r.Use(middleware.RequestID)
	r.Use(middleware.Recoverer)
	r.Use(middleware.Logger)

	r.Get("/", s.handleIndex)
	r.Get("/clip", s.handleClipGet)
	r.Post("/clip", s.handleClipSet)
	r.Post("/files", s.handleUpload)
	r.Get("/files/{id}", s.handleDownload)
	r.Delete("/files/{id}", s.handleDelete)

	staticSub, err := fs.Sub(staticFS, "static")
	if err != nil {
		log.Fatalf("static fs: %v", err)
	}
	r.Handle("/static/*", http.StripPrefix("/static/", http.FileServer(http.FS(staticSub))))

	return r
}

func (s *server) device(r *http.Request) string {
	if s.cfg.WhoIs == nil {
		return "dev"
	}
	if name := s.cfg.WhoIs(r.Context(), r.RemoteAddr); name != "" {
		return name
	}
	return "unknown"
}

type indexData struct {
	Clip  clipView
	Files []fileView
}

func (s *server) handleIndex(w http.ResponseWriter, r *http.Request) {
	clip, err := s.cfg.DB.GetClip(r.Context())
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	files, err := s.cfg.DB.ListFiles(r.Context())
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	data := indexData{Clip: newClipView(clip), Files: make([]fileView, 0, len(files))}
	for _, f := range files {
		data.Files = append(data.Files, newFileView(f))
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if err := s.tpl.ExecuteTemplate(w, "index.html", data); err != nil {
		log.Printf("render index: %v", err)
	}
}

func (s *server) handleClipGet(w http.ResponseWriter, r *http.Request) {
	clip, err := s.cfg.DB.GetClip(r.Context())
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	s.renderClip(w, newClipView(clip))
}

func (s *server) handleClipSet(w http.ResponseWriter, r *http.Request) {
	if err := r.ParseForm(); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	text := r.FormValue("text")
	now := time.Now()
	if err := s.cfg.DB.SetClip(r.Context(), text, s.device(r), now); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	s.renderClip(w, newClipView(store.Clip{Text: text, UpdatedAt: now, UpdatedBy: s.device(r)}))
}

func (s *server) renderClip(w http.ResponseWriter, v clipView) {
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if err := s.tpl.ExecuteTemplate(w, "clip", v); err != nil {
		log.Printf("render clip: %v", err)
	}
}

func (s *server) handleUpload(w http.ResponseWriter, r *http.Request) {
	r.Body = http.MaxBytesReader(w, r.Body, s.cfg.MaxUpload)
	if err := r.ParseMultipartForm(8 << 20); err != nil {
		http.Error(w, "upload too large or malformed: "+err.Error(), http.StatusBadRequest)
		return
	}
	file, header, err := r.FormFile("file")
	if err != nil {
		http.Error(w, "missing file: "+err.Error(), http.StatusBadRequest)
		return
	}
	defer file.Close()

	id, err := newID()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	blobPath := filepath.Join(s.cfg.BlobDir, id)
	dst, err := os.OpenFile(blobPath, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	written, copyErr := io.Copy(dst, file)
	closeErr := dst.Close()
	if copyErr != nil || closeErr != nil {
		_ = os.Remove(blobPath)
		err := copyErr
		if err == nil {
			err = closeErr
		}
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	mime := header.Header.Get("Content-Type")
	if mime == "" {
		mime = "application/octet-stream"
	}
	now := time.Now()
	f := store.File{
		ID:         id,
		Name:       header.Filename,
		Mime:       mime,
		Size:       written,
		Uploader:   s.device(r),
		UploadedAt: now,
		ExpiresAt:  now.Add(s.cfg.TTL),
	}
	if err := s.cfg.DB.InsertFile(r.Context(), f); err != nil {
		_ = os.Remove(blobPath)
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if err := s.tpl.ExecuteTemplate(w, "file_row", newFileView(f)); err != nil {
		log.Printf("render file_row: %v", err)
	}
}

func (s *server) handleDownload(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	f, err := s.cfg.DB.GetFile(r.Context(), id)
	if errors.Is(err, store.ErrNotFound) {
		http.NotFound(w, r)
		return
	}
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	blobPath := filepath.Join(s.cfg.BlobDir, id)
	file, err := os.Open(blobPath)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	defer file.Close()
	w.Header().Set("Content-Type", f.Mime)
	w.Header().Set("Content-Length", fmt.Sprintf("%d", f.Size))
	w.Header().Set("Content-Disposition", fmt.Sprintf(`attachment; filename="%s"`, sanitizeFilename(f.Name)))
	http.ServeContent(w, r, f.Name, f.UploadedAt, file)
}

func (s *server) handleDelete(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := s.cfg.DB.DeleteFile(r.Context(), id); err != nil && !errors.Is(err, store.ErrNotFound) {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	_ = os.Remove(filepath.Join(s.cfg.BlobDir, id))
	w.WriteHeader(http.StatusOK)
}

// RunSweeper deletes expired files on a 1-minute tick until ctx is canceled.
func RunSweeper(ctx context.Context, cfg Config) {
	t := time.NewTicker(time.Minute)
	defer t.Stop()
	sweep(ctx, cfg)
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			sweep(ctx, cfg)
		}
	}
}

func sweep(ctx context.Context, cfg Config) {
	files, err := cfg.DB.ExpiredFiles(ctx, time.Now())
	if err != nil {
		log.Printf("sweep list: %v", err)
		return
	}
	for _, f := range files {
		if err := cfg.DB.DeleteFile(ctx, f.ID); err != nil {
			log.Printf("sweep delete row %s: %v", f.ID, err)
			continue
		}
		_ = os.Remove(filepath.Join(cfg.BlobDir, f.ID))
		log.Printf("sweep expired %s (%s)", f.ID, f.Name)
	}
}

func newID() (string, error) {
	var b [12]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(b[:]), nil
}

func sanitizeFilename(name string) string {
	// Replace double-quotes and control characters in Content-Disposition.
	out := make([]rune, 0, len(name))
	for _, r := range name {
		if r == '"' || r < 0x20 {
			out = append(out, '_')
			continue
		}
		out = append(out, r)
	}
	return string(out)
}
