// The entry point for the PUBLIC site. Deliberately small: a reader needs
// nothing but Turbo.
//
// The editor lives in admin.js, which imports this file and then adds Lexxy on
// top. See config/importmap.rb for why the two are split.
import "@hotwired/turbo-rails"
import "controllers"
