# Pin npm packages by running ./bin/importmap

# Shared by both entry points.
pin "application"
pin "@hotwired/turbo-rails", to: "turbo.min.js"
pin "@hotwired/stimulus", to: "stimulus.min.js"
pin "@hotwired/stimulus-loading", to: "stimulus-loading.js"
pin_all_from "app/javascript/controllers", under: "controllers"

# The admin-only entry point. The layout picks between this and "application"
# by hostname, so a public reader never loads the editor — see below for why
# that is worth the extra file.
pin "admin"

# Lexxy replaces Trix as the Action Text editor. `action_text:install` pins
# "trix" here; it is deliberately absent.
#
# Removing it was checked rather than assumed: lexxy.js never imports "trix"
# (its only "trix" substrings are inside the word "matrix"), and
# actiontext.esm.js does not `import "trix"` either — it only listens for a
# `trix-attachment-add` event, which is an event name, not a module. Nothing
# left in this app can request "trix", so the pin would be dead config implying
# a dependency we do not have.
#
# `preload: false` ON ALL THREE IS THE POINT. lexxy.js is 933 KB, and it powers
# the EDITOR only — it defines no behaviour for published content, so a reader
# of the public site would download it and get nothing. Preloading is on by
# default, which would fetch it on every page regardless of who imports it.
pin "lexxy", to: "lexxy.js", preload: false
pin "@rails/actiontext", to: "actiontext.esm.js", preload: false

# Required by lexxy.js, which reaches for it with a *dynamic* import at the
# moment a file is attached:
#
#     const { DirectUpload } = await import('@rails/activestorage')
#
# Grepping a bundle for `import ... from "..."` does not find that, which is
# how this pin came to be missing. Because the import is dynamic rather than
# static, an absent pin does not fail at load time — the module graph links,
# the editor renders, and everything looks fine until the first attachment,
# when the specifier fails to resolve and the await rejects. DirectUpload is
# never constructed, so no request is ever made; the upload placeholder sits at
# 0% forever and Lexxy's `setCustomValidity` guard blocks the form with "Please
# wait for all files to upload", permanently.
#
# So: any bundle pinned here must be checked for dynamic imports too, not just
# static ones.
pin "@rails/activestorage", to: "activestorage.esm.js", preload: false
