# sonar

Replays one captured chatbot request against a whole list of prompts, routed
through an intercepting proxy. A local CLI, not a fleet service — there is no
`deploy.toml` on purpose.

The workflow mirrors Burp Intruder, but scriptable and reply-aware: capture a
single request out of the proxy, mark where the user message goes, hand sonar a
prompt list, and it fires a faithful copy of that request per prompt — same
headers, same auth, same body shape — sends every hit back through the proxy so
the traffic lands in its history, extracts the assistant's reply, and writes a
complete JSON results file.

```
sonar --request req.txt --prompts prompts.txt --proxy http://127.0.0.1:8080 --insecure
sonar --request req.txt --prompts prompts.json --proxy http://127.0.0.1:8080 --proxy-ca burp-ca.der
sonar --request req.txt --prompts prompts.txt --json > results.json
```

## The request template

Save a request from the proxy (in Burp: right-click → *Copy to file*, or
Repeater's *Save item*) and drop the marker `§PROMPT§` where the message goes:

```
POST /api/chat HTTP/1.1
Host: target.example
Authorization: Bearer eyJ...
Content-Type: application/json

{"messages":[{"role":"user","content":"§PROMPT§"}]}
```

The marker is position-blind — it can sit in the body, a header, or the request
line. The scheme is not recorded in a saved request, so it defaults to `https`
(`--scheme http` to override), and the URL is built from the `Host` header.
`Content-Length`, `Host`, and the transfer/encoding headers are recomputed by
the client, so a stale `Content-Length` in the capture does no harm.

By default each prompt is **JSON-escaped** before substitution, because the
marker almost always lands inside a JSON string literal — an unescaped quote or
newline would otherwise corrupt the body. Use `--escape url` for a marker in a
query string or form body, or `--escape none` to splice verbatim.

## The prompt list

- `*.json` — a JSON array of strings. Use this when prompts span multiple lines
  or carry awkward characters.
- anything else — a wordlist: one prompt per line, blank lines and `#` comments
  ignored.

## Trusting the proxy's TLS

A proxy like Burp forges a leaf certificate per host from its own CA, which the
system store does not know, so every HTTPS target fails ordinary verification.
Two ways to fix that, mutually exclusive:

- `--proxy-ca <file>` — add Burp's exported CA (PEM or DER) to the trust store,
  keeping full verification. Export it once from *Proxy → Options → Import /
  export CA certificate*.
- `--insecure` — skip certificate verification, scoped to this client only.
  Convenient for a proxy you control.

## Reading the reply

The response body is read as `text/event-stream` or JSON based on its
`Content-Type`, and the assistant's text is pulled from the common
provider-specific paths (OpenAI-style `choices[].message.content`, a bare
`reply`, an SSE `delta.content`, and so on). When a target does something
unusual, name the field explicitly:

- `--reply-pointer /data/answer` — JSON pointer to the reply in a JSON body.
- `--delta-pointer /choices/0/delta/text` — JSON pointer to the per-chunk delta
  in an SSE stream.

The full untouched body is always kept in the results file regardless, so a
missed heuristic never loses data.

## Pacing

`--concurrency` defaults to 1, which keeps the proxy history strictly ordered;
raise it to fire in parallel. `--delay <ms>` spaces successive dispatches.
`--timeout <secs>` (default 120) bounds each request — model responses are slow.

## Output

The console shows a compact per-prompt digest (status, latency, prompt and
reply snippets). The full record — status, headers' content-type, extracted
reply, and the complete response body for every prompt — is written to
`sonar-results.json` (`--out` to change), or to stdout with `--json`.
