source "https://rubygems.org"

# Bundle edge Rails instead: gem "rails", github: "rails/rails", branch: "main"
gem "rails", "~> 8.1.3", ">= 8.1.3.1"
# The modern asset pipeline for Rails [https://github.com/rails/propshaft]
gem "propshaft"
# Use sqlite3 as the database for Active Record
gem "sqlite3", ">= 2.1"
# Use the Puma web server [https://github.com/puma/puma]
gem "puma", ">= 5.0"
# Use JavaScript with ESM import maps [https://github.com/rails/importmap-rails]
gem "importmap-rails"
# Hotwire's SPA-like page accelerator [https://turbo.hotwired.dev]
gem "turbo-rails"
# Hotwire's modest JavaScript framework [https://stimulus.hotwired.dev]
gem "stimulus-rails"

# Use Active Model has_secure_password [https://guides.rubyonrails.org/active_model_basics.html#securepassword]
# gem "bcrypt", "~> 3.1.7"

# Windows does not include zoneinfo files, so bundle the tzinfo-data gem
gem "tzinfo-data", platforms: %i[ windows jruby ]

# Reduces boot times through caching; required in config/boot.rb
gem "bootsnap", require: false

# Add HTTP asset caching/compression and X-Sendfile acceleration to Puma [https://github.com/basecamp/thruster/]
gem "thruster", require: false

# Rich text editing, the same stack the notes app uses. Lexxy is an Action Text
# editor, so post bodies are Action Text rich text rather than a markdown column.
gem "lexxy", "~> 0.9.21"

# Active Storage variants, for images attached inside a post body. Action Text
# attachments go through Active Storage, and Lexxy's drag-and-drop/paste path
# leans on it — so this comes back together with libvips in the Dockerfile,
# exactly as the comment there said it should.
gem "image_processing", "~> 1.2"

# kramdown is retained ONLY to convert the pre-Lexxy markdown bodies in
# db/migrate/*_convert_post_bodies_to_rich_text.rb. Nothing at runtime renders
# markdown any more. It can be dropped once that migration is squashed away.
gem "kramdown", "~> 2.5"
gem "kramdown-parser-gfm", "~> 1.1"

# Syntax highlighting for the code blocks in a post, done here rather than in
# the browser.
#
# Lexxy highlights with Prism, out of the 933KB bundle that also carries the
# editor — and that bundle is loaded by admin.js alone, because a reader needs
# nothing but Turbo. So the published page has been serving code as plain text
# while the token palette in application.css styled only the admin. Rouge does
# the same job in Ruby, inside the fragment cache the body already sits in, and
# leaves the public page with no JavaScript to load.
gem "rouge", "~> 4.5"

group :development, :test do
  # See https://guides.rubyonrails.org/debugging_rails_applications.html#debugging-with-the-debug-gem
  gem "debug", platforms: %i[ mri windows ], require: "debug/prelude"

  # Audits gems for known security defects (use config/bundler-audit.yml to ignore issues)
  gem "bundler-audit", require: false

  # Static analysis for security vulnerabilities [https://brakemanscanner.org/]
  gem "brakeman", require: false

  # Omakase Ruby styling [https://github.com/rails/rubocop-rails-omakase/]
  gem "rubocop-rails-omakase", require: false
end

group :development do
  # Use console on exceptions pages [https://github.com/rails/web-console]
  gem "web-console"
end

group :test do
  # Use system testing [https://guides.rubyonrails.org/testing.html#system-testing]
  gem "capybara"
  gem "selenium-webdriver"
end
