import type { DeployStatus, ServiceStatus } from "../types.ts";
import {
  cn,
  deployVerdictLabel,
  deployVerdictStyle,
  statusColor,
} from "../lib/utils.ts";

interface Props {
  service: ServiceStatus;
  /** Deploy freshness, or null when the service isn't deployable. */
  deploy: DeployStatus | null;
  /** Open this service's detail (logs / deploy) view. */
  onSelect: () => void;
}

/** One service on the fleet home: live status + what it is + where to reach it,
 * with the detail view (logs/deploy) one click away. */
export function FleetCard({ service, deploy, onSelect }: Props) {
  const color = statusColor(service.active_state);
  const deployStyle = deploy ? deployVerdictStyle(deploy.verdict) : null;
  const summary = service.fleet?.summary || service.description;
  const url = service.fleet?.url;
  const docsUrl = service.fleet?.docs_url;
  const port = service.fleet?.port;

  return (
    <section className="flex flex-col border border-rule bg-surface p-4 hover:border-rule-strong">
      <header className="flex items-baseline justify-between gap-2">
        <button
          type="button"
          onClick={onSelect}
          className="min-w-0 truncate text-left font-medium text-ink hover:text-accent"
        >
          {service.name}
        </button>
        <span className="flex shrink-0 items-center gap-2 text-[11px]">
          <span className={cn("h-2 w-2 shrink-0", color.dot)} />
          <span className={cn("uppercase tracking-wide", color.text)}>
            {service.active_state}
            {service.sub_state ? ` · ${service.sub_state}` : ""}
          </span>
        </span>
      </header>

      {summary && (
        <p className="mt-1 line-clamp-2 text-xs text-ink-muted">{summary}</p>
      )}

      <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-ink-faint">
        <span>{service.unit}</span>
        {port != null && <span>:{port}</span>}
        {service.since && <span className="truncate">since {service.since}</span>}
        {deploy && deployStyle && (
          <span className={cn("flex items-center gap-1", deployStyle.text)}>
            <span className={cn("h-1.5 w-1.5 rounded-full", deployStyle.dot)} />
            {deployVerdictLabel(deploy.verdict, deploy.local?.undeployed_commits ?? null)}
          </span>
        )}
      </div>

      <div className="mt-3 flex items-center gap-3 border-t border-rule pt-3 text-xs">
        {url ? (
          <a
            href={url}
            target="_blank"
            rel="noreferrer"
            className="text-accent hover:underline"
          >
            open ↗
          </a>
        ) : (
          <span className="text-ink-faint">no route</span>
        )}
        {docsUrl && (
          <a
            href={docsUrl}
            target="_blank"
            rel="noreferrer"
            className="text-accent hover:underline"
          >
            docs
          </a>
        )}
        <button
          type="button"
          onClick={onSelect}
          className="ml-auto text-ink-muted hover:text-accent"
        >
          manage ›
        </button>
      </div>
    </section>
  );
}
