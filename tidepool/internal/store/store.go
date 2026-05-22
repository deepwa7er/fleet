package store

import (
	"context"
	"database/sql"
	"errors"
	"time"

	_ "modernc.org/sqlite"
)

const schema = `
CREATE TABLE IF NOT EXISTS files (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    mime        TEXT NOT NULL,
    size        INTEGER NOT NULL,
    uploader    TEXT NOT NULL,
    uploaded_at INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_files_expires ON files(expires_at);

CREATE TABLE IF NOT EXISTS clip (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    text       TEXT NOT NULL DEFAULT '',
    updated_at INTEGER NOT NULL DEFAULT 0,
    updated_by TEXT NOT NULL DEFAULT ''
);
INSERT OR IGNORE INTO clip (id, text, updated_at, updated_by) VALUES (1, '', 0, '');
`

type DB struct{ *sql.DB }

func Open(path string) (*DB, error) {
	db, err := sql.Open("sqlite", path+"?_pragma=journal_mode(WAL)&_pragma=busy_timeout(5000)&_pragma=foreign_keys(1)")
	if err != nil {
		return nil, err
	}
	if _, err := db.Exec(schema); err != nil {
		_ = db.Close()
		return nil, err
	}
	return &DB{db}, nil
}

type File struct {
	ID         string
	Name       string
	Mime       string
	Size       int64
	Uploader   string
	UploadedAt time.Time
	ExpiresAt  time.Time
}

func (d *DB) InsertFile(ctx context.Context, f File) error {
	_, err := d.ExecContext(ctx,
		`INSERT INTO files (id, name, mime, size, uploader, uploaded_at, expires_at) VALUES (?,?,?,?,?,?,?)`,
		f.ID, f.Name, f.Mime, f.Size, f.Uploader, f.UploadedAt.Unix(), f.ExpiresAt.Unix(),
	)
	return err
}

func (d *DB) ListFiles(ctx context.Context) ([]File, error) {
	rows, err := d.QueryContext(ctx,
		`SELECT id, name, mime, size, uploader, uploaded_at, expires_at FROM files ORDER BY uploaded_at DESC`,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []File
	for rows.Next() {
		var f File
		var up, ex int64
		if err := rows.Scan(&f.ID, &f.Name, &f.Mime, &f.Size, &f.Uploader, &up, &ex); err != nil {
			return nil, err
		}
		f.UploadedAt = time.Unix(up, 0)
		f.ExpiresAt = time.Unix(ex, 0)
		out = append(out, f)
	}
	return out, rows.Err()
}

var ErrNotFound = errors.New("not found")

func (d *DB) GetFile(ctx context.Context, id string) (File, error) {
	var f File
	var up, ex int64
	err := d.QueryRowContext(ctx,
		`SELECT id, name, mime, size, uploader, uploaded_at, expires_at FROM files WHERE id = ?`, id,
	).Scan(&f.ID, &f.Name, &f.Mime, &f.Size, &f.Uploader, &up, &ex)
	if errors.Is(err, sql.ErrNoRows) {
		return f, ErrNotFound
	}
	if err != nil {
		return f, err
	}
	f.UploadedAt = time.Unix(up, 0)
	f.ExpiresAt = time.Unix(ex, 0)
	return f, nil
}

func (d *DB) DeleteFile(ctx context.Context, id string) error {
	res, err := d.ExecContext(ctx, `DELETE FROM files WHERE id = ?`, id)
	if err != nil {
		return err
	}
	if n, _ := res.RowsAffected(); n == 0 {
		return ErrNotFound
	}
	return nil
}

// ExpiredFiles returns rows whose expires_at has passed.
func (d *DB) ExpiredFiles(ctx context.Context, now time.Time) ([]File, error) {
	rows, err := d.QueryContext(ctx,
		`SELECT id, name, mime, size, uploader, uploaded_at, expires_at FROM files WHERE expires_at <= ?`,
		now.Unix(),
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []File
	for rows.Next() {
		var f File
		var up, ex int64
		if err := rows.Scan(&f.ID, &f.Name, &f.Mime, &f.Size, &f.Uploader, &up, &ex); err != nil {
			return nil, err
		}
		f.UploadedAt = time.Unix(up, 0)
		f.ExpiresAt = time.Unix(ex, 0)
		out = append(out, f)
	}
	return out, rows.Err()
}

type Clip struct {
	Text      string
	UpdatedAt time.Time
	UpdatedBy string
}

func (d *DB) GetClip(ctx context.Context) (Clip, error) {
	var c Clip
	var t int64
	err := d.QueryRowContext(ctx,
		`SELECT text, updated_at, updated_by FROM clip WHERE id = 1`,
	).Scan(&c.Text, &t, &c.UpdatedBy)
	if err != nil {
		return c, err
	}
	if t > 0 {
		c.UpdatedAt = time.Unix(t, 0)
	}
	return c, nil
}

func (d *DB) SetClip(ctx context.Context, text, by string, at time.Time) error {
	_, err := d.ExecContext(ctx,
		`UPDATE clip SET text = ?, updated_at = ?, updated_by = ? WHERE id = 1`,
		text, at.Unix(), by,
	)
	return err
}
