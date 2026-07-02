package server

import (
	"sync"

	"tidepool/internal/store"
)

type clipHub struct {
	mu   sync.Mutex
	subs map[chan store.Clip]struct{}
}

func newClipHub() *clipHub {
	return &clipHub{subs: make(map[chan store.Clip]struct{})}
}

func (h *clipHub) subscribe() chan store.Clip {
	ch := make(chan store.Clip, 4)
	h.mu.Lock()
	h.subs[ch] = struct{}{}
	h.mu.Unlock()
	return ch
}

func (h *clipHub) unsubscribe(ch chan store.Clip) {
	h.mu.Lock()
	delete(h.subs, ch)
	h.mu.Unlock()
}

func (h *clipHub) publish(c store.Clip) {
	h.mu.Lock()
	defer h.mu.Unlock()
	for ch := range h.subs {
		select {
		case ch <- c:
		default:
		}
	}
}
