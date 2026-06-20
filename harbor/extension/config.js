// Where the harbor server lives. Change this for your environment:
//   VPS (tailnet) : https://harbor.internal.deepwa7er.com   (HTTPS via breakwater,
//                   the tailnet front door; reachable from any tailnet device)
//   local dev     : http://127.0.0.1:8090
//
// Whatever you put here must also be listed in manifest.json `host_permissions`.
window.HARBOR = {
  api: "https://harbor.internal.deepwa7er.com",
};
