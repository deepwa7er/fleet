.PHONY: run dev build css css-watch tailwind-install icons clean

# --- build / run ---

build: css
	go build -o tidepool .

dev: css
	go run . -dev

run: build
	./tidepool

# --- tailwind ---

# Output is embedded into the binary on next build.
css:
	./tailwindcss -i web/input.css -o internal/server/static/app.css --minify

css-watch:
	./tailwindcss -i web/input.css -o internal/server/static/app.css --watch

# One-shot: download the standalone Tailwind CLI (linux-x64) into ./tailwindcss
TAILWIND_VERSION ?= v3.4.13
TAILWIND_URL = https://github.com/tailwindlabs/tailwindcss/releases/download/$(TAILWIND_VERSION)/tailwindcss-linux-x64
tailwind-install:
	curl -fL $(TAILWIND_URL) -o tailwindcss
	chmod +x tailwindcss
	@echo "Installed tailwindcss $(TAILWIND_VERSION)"

# --- placeholder icons (requires imagemagick) ---

icons:
	convert -size 512x512 xc:'#0f172a' -fill '#7dd3fc' -gravity center \
	  -pointsize 220 -annotate 0 'tp' internal/server/static/icon-512.png
	convert internal/server/static/icon-512.png -resize 192x192 internal/server/static/icon-192.png

clean:
	rm -f tidepool internal/server/static/app.css
	rm -rf data tsnet-state
