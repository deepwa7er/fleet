package server

import (
	"fmt"
	"html/template"
	"time"

	"tidepool/internal/store"
)

type clipView struct {
	Text      string
	UpdatedAt time.Time
	UpdatedBy string
}

func newClipView(c store.Clip) clipView {
	return clipView{Text: c.Text, UpdatedAt: c.UpdatedAt, UpdatedBy: c.UpdatedBy}
}

func (v clipView) HasUpdate() bool { return !v.UpdatedAt.IsZero() }

func (v clipView) UpdatedAtRel() string {
	if v.UpdatedAt.IsZero() {
		return "never"
	}
	return relPast(v.UpdatedAt, time.Now())
}

type fileView struct {
	ID           string
	Name         string
	Uploader     string
	SizeHuman    string
	ExpiresInRel string
}

func newFileView(f store.File) fileView {
	return fileView{
		ID:           f.ID,
		Name:         f.Name,
		Uploader:     f.Uploader,
		SizeHuman:    humanSize(f.Size),
		ExpiresInRel: relFuture(f.ExpiresAt, time.Now()),
	}
}

var funcs = template.FuncMap{}

func humanSize(b int64) string {
	const k = 1024.0
	switch {
	case b < int64(k):
		return fmt.Sprintf("%d B", b)
	case b < int64(k*k):
		return fmt.Sprintf("%.1f KB", float64(b)/k)
	case b < int64(k*k*k):
		return fmt.Sprintf("%.1f MB", float64(b)/(k*k))
	default:
		return fmt.Sprintf("%.1f GB", float64(b)/(k*k*k))
	}
}

func relPast(t, now time.Time) string {
	d := now.Sub(t)
	if d < time.Minute {
		return "just now"
	}
	if d < time.Hour {
		return fmt.Sprintf("%dm ago", int(d.Minutes()))
	}
	if d < 24*time.Hour {
		return fmt.Sprintf("%dh ago", int(d.Hours()))
	}
	return fmt.Sprintf("%dd ago", int(d.Hours()/24))
}

func relFuture(t, now time.Time) string {
	d := t.Sub(now)
	if d <= 0 {
		return "expired"
	}
	if d < time.Hour {
		return fmt.Sprintf("in %dm", int(d.Minutes()))
	}
	if d < 24*time.Hour {
		return fmt.Sprintf("in %dh", int(d.Hours()))
	}
	return fmt.Sprintf("in %dd", int(d.Hours()/24))
}
