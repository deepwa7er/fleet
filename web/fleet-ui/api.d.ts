/**
 * Perform a JSON request against a fleet API. Throws `Error(message)` using
 * the service's `{"error": …}` body when the response is not OK; resolves to
 * `undefined` on 204.
 */
export function req<T>(method: string, path: string, body?: unknown): Promise<T>;
