// bridge/lib/errors.js
// The one error type the HTTP layer turns into a response. Harness adapters
// throw it for client-visible failures (unknown session, unsupported
// capability, a harness that rejected the operation); anything else that
// escapes is a 500 "internal error" — see server.js.

export class HttpError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}
