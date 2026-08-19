// bridge/lib/jsonl.js
// JSONL stream discipline shared by every harness that reads a child
// process's stdout: split on "\n" only (Node's readline also splits on
// U+2028/U+2029, which are legal inside JSON strings, so the splitter is
// hand-rolled), strip one trailing "\r", and decode with StringDecoder so a
// multi-byte UTF-8 sequence split across chunks cannot corrupt a record.

import { StringDecoder } from "node:string_decoder";

export function createJsonlReader(stream, onLine, onEnd = () => {}, onError = () => {}) {
  const decoder = new StringDecoder("utf8");
  let buffer = "";
  stream.on("data", (chunk) => {
    buffer += decoder.write(chunk);
    let nl;
    while ((nl = buffer.indexOf("\n")) !== -1) {
      let line = buffer.slice(0, nl);
      buffer = buffer.slice(nl + 1);
      if (line.endsWith("\r")) line = line.slice(0, -1);
      onLine(line);
    }
  });
  stream.on("end", () => {
    buffer += decoder.end();
    if (buffer.length > 0) onLine(buffer.endsWith("\r") ? buffer.slice(0, -1) : buffer);
    onEnd();
  });
  stream.on("error", onError);
  return stream;
}
