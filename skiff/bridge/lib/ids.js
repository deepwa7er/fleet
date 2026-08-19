// bridge/lib/ids.js
// Session ids on the wire are harness-qualified: "pi:<file basename>",
// "muse:<uuid>", "opencode:<ses_…>". The prefix is what routes a request to
// its harness; the local part is whatever that harness natively calls the
// session. The colon is safe in a URL path segment (RFC 3986 pchar) and none
// of the three harnesses use it in their own ids, so splitting on the first
// colon is unambiguous.

export function formatSessionId(harness, localId) {
  return `${harness}:${localId}`;
}

// "pi:abc" -> { harness: "pi", localId: "abc" }; anything without a colon or
// with an empty half is not a session id and resolves to null (callers 404).
export function parseSessionId(id) {
  const colon = id.indexOf(":");
  if (colon <= 0 || colon === id.length - 1) return null;
  return { harness: id.slice(0, colon), localId: id.slice(colon + 1) };
}
