// The fleet's JSON API fetch wrapper: every service responds with either the
// requested JSON or the uniform `{"error": message}` body (see fleet-common's
// http module) — this is the client side of that contract.

/**
 * Perform a JSON request against a fleet API. Throws `Error(message)` using
 * the service's `{"error": …}` body when the response is not OK; resolves to
 * `undefined` on 204.
 *
 * @template T
 * @param {string} method
 * @param {string} path
 * @param {unknown} [body]
 * @returns {Promise<T>}
 */
export async function req(method, path, body) {
  const res = await fetch(path, {
    method,
    headers: body !== undefined ? { "content-type": "application/json" } : undefined,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) {
    let message = `request failed (${res.status})`;
    try {
      const data = await res.json();
      if (data?.error) message = data.error;
    } catch {
      /* keep default */
    }
    throw new Error(message);
  }
  if (res.status === 204) return undefined;
  return await res.json();
}
