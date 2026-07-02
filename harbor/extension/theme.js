// harbor: follow the fleet-wide theme from tide. An extension new-tab page can't
// read the shared *.intern.deepwa7er.net cookie the served UIs use, so first
// paint comes from a localStorage cache; then we fetch the source of truth and
// poll so the tab flips live when `b dark` / `b light` changes it. Loaded early
// in <head> to minimize flash. See github.com/deepwa7er/tide.
(function () {
  var TIDE = "https://tide.intern.deepwa7er.net/theme";
  function apply(theme) {
    var el = document.documentElement;
    el.dataset.theme = theme;
    el.style.colorScheme = theme;
  }
  try {
    apply(localStorage.getItem("fleet_theme") || "dark");
  } catch (_) {}
  function sync() {
    fetch(TIDE, { cache: "no-store" })
      .then(function (r) { return r.ok ? r.json() : null; })
      .then(function (d) {
        if (d && (d.theme === "dark" || d.theme === "light")) {
          apply(d.theme);
          try { localStorage.setItem("fleet_theme", d.theme); } catch (_) {}
        }
      })
      .catch(function () {});
  }
  sync();
  setInterval(sync, 5000);
})();
