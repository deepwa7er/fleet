// A symbol kind, colored by the DG-002 §5 role system: functions are actions
// (accent), types are shapes (lcd-ink), values are names (ink).

const LABELS: Record<string, string> = {
  function: "fn",
  method: "method",
  static_method: "static fn",
  trait_method: "trait fn",
  macro: "macro",
  struct: "struct",
  enum: "enum",
  enum_member: "variant",
  trait: "trait",
  type_alias: "type",
  assoc_type: "assoc type",
  type: "type",
  field: "field",
  constant: "const",
  static: "static",
  value: "value",
  module: "mod",
};

export function kindRole(kind: string): "fn" | "type" | "value" | "other" {
  switch (kind) {
    case "function":
    case "method":
    case "static_method":
    case "trait_method":
    case "macro":
      return "fn";
    case "struct":
    case "enum":
    case "enum_member":
    case "trait":
    case "type_alias":
    case "assoc_type":
    case "type":
      return "type";
    case "field":
    case "constant":
    case "static":
    case "value":
      return "value";
    default:
      return "other";
  }
}

export function KindTag({ kind }: { kind: string }) {
  return <span className={`kind kind--${kindRole(kind)}`}>{LABELS[kind] ?? kind}</span>;
}
