JAR := dark2026-theme.jar

.PHONY: jar install clean

jar: $(JAR)

$(JAR): META-INF/plugin.xml theme/dark2026.theme.json theme/dark2026.editor.xml
	rm -f $@
	zip -qr $@ META-INF theme

install: $(JAR)
	@dest=$$(ls -d "$$HOME/Library/Application Support/JetBrains"/RubyMine*/plugins 2>/dev/null | sort -V | tail -1); \
	if [ -z "$$dest" ]; then echo "no RubyMine plugins directory found under ~/Library/Application Support/JetBrains" >&2; exit 1; fi; \
	cp $(JAR) "$$dest/" && echo "installed $(JAR) -> $$dest (restart RubyMine)"

clean:
	rm -f $(JAR)
