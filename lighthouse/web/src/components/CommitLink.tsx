import { cn } from "../lib/utils.ts";

interface Props {
  /** Short sha to display. */
  short: string;
  /** GitHub commit URL, or null when the repo isn't on GitHub. */
  url: string | null;
  /** Extra classes (e.g. the surrounding text color). */
  className?: string;
}

/** A commit hash: a link to the commit on GitHub when the repo's web URL is
 *  known, otherwise plain monospace text. Stops click propagation so it stays a
 *  link even inside a clickable row (e.g. the deploy history). */
export function CommitLink({ short, url, className }: Props) {
  const base = cn("font-mono", className);
  if (!url) {
    return <span className={base}>{short}</span>;
  }
  return (
    <a
      href={url}
      target="_blank"
      rel="noopener noreferrer"
      title="View this commit on GitHub"
      onClick={(e) => e.stopPropagation()}
      className={cn(
        base,
        "underline decoration-dotted underline-offset-2 hover:decoration-solid",
      )}
    >
      {short}
    </a>
  );
}
