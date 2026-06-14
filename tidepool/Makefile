.PHONY: run dev build icons clean

# --- build / run ---

build:
	go build -o tidepool .

dev:
	go run . -dev

run: build
	./tidepool

# --- placeholder icons (requires imagemagick) ---

icons:
	convert -size 512x512 xc:'#1a1a1a' -fill '#e8590c' -gravity center \
	  -pointsize 220 -annotate 0 'tp' internal/server/static/icon-512.png
	convert internal/server/static/icon-512.png -resize 192x192 internal/server/static/icon-192.png

clean:
	rm -f tidepool
	rm -rf data tsnet-state
