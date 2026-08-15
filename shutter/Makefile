APP     := Shutter
BUNDLE  := $(APP).app
CONFIG  := release
BIN     := .build/$(CONFIG)/$(APP)

.PHONY: build app run install clean

build:
	swift build -c $(CONFIG)

# Assemble a proper .app bundle, signed with a stable identity so the Screen
# Recording grant survives rebuilds (ad-hoc signing ties TCC's record to the
# cdhash, which changes on every build, and the permission has to be re-granted).
app: build
	rm -rf $(BUNDLE)
	mkdir -p $(BUNDLE)/Contents/MacOS
	cp $(BIN) $(BUNDLE)/Contents/MacOS/$(APP)
	cp Resources/Info.plist $(BUNDLE)/Contents/Info.plist
	codesign --force --sign "Apple Development: Joseph Mafrici (2H496SGC5C)" --timestamp=none $(BUNDLE)
	@echo "Built $(BUNDLE)"

run: app
	open $(BUNDLE)

# Install into ~/Applications for everyday use. Staged and swapped in with two
# renames rather than rm -rf + a slow recursive copy: the login-item record is
# keyed on this path, and a bundle missing for the length of a copy can get the
# registration invalidated.
install: app
	mkdir -p $(HOME)/Applications
	rm -rf $(HOME)/Applications/$(BUNDLE).new $(HOME)/Applications/$(BUNDLE).old
	cp -R $(BUNDLE) $(HOME)/Applications/$(BUNDLE).new
	@if [ -e $(HOME)/Applications/$(BUNDLE) ]; then \
		mv $(HOME)/Applications/$(BUNDLE) $(HOME)/Applications/$(BUNDLE).old; \
	fi
	mv $(HOME)/Applications/$(BUNDLE).new $(HOME)/Applications/$(BUNDLE)
	rm -rf $(HOME)/Applications/$(BUNDLE).old
	@echo "Installed to ~/Applications/$(BUNDLE)"

clean:
	rm -rf .build $(BUNDLE)
