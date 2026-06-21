// Where the harbor server lives. Change this for your environment:
//   VPS (tailnet) : https://harbor.internal.deepwa7er.com
//   local dev     : http://127.0.0.1:8090
//
// Whatever you put here must also be listed in manifest.json `host_permissions`.
window.HARBOR = {
  api: "https://harbor.internal.deepwa7er.com",
  lagoon_url: "https://lagoon.internal.deepwa7er.com",
};
