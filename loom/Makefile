APP     := Carousel
BUNDLE  := $(APP).app
CONFIG  := release
BIN     := .build/$(CONFIG)/$(APP)

.PHONY: build app run install clean

build:
	swift build -c $(CONFIG)

# Assemble a proper .app bundle, signed with a stable identity so the
# Accessibility grant survives rebuilds (ad-hoc signing ties it to the cdhash).
app: build
	rm -rf $(BUNDLE)
	mkdir -p $(BUNDLE)/Contents/MacOS
	cp $(BIN) $(BUNDLE)/Contents/MacOS/$(APP)
	cp Resources/Info.plist $(BUNDLE)/Contents/Info.plist
	codesign --force --sign "Apple Development: Joseph Mafrici (2H496SGC5C)" --timestamp=none $(BUNDLE)
	@echo "Built $(BUNDLE)"

run: app
	open $(BUNDLE)

# Install into ~/Applications for everyday use.
install: app
	mkdir -p $(HOME)/Applications
	rm -rf $(HOME)/Applications/$(BUNDLE)
	cp -R $(BUNDLE) $(HOME)/Applications/$(BUNDLE)
	@echo "Installed to ~/Applications/$(BUNDLE)"

clean:
	rm -rf .build $(BUNDLE)
