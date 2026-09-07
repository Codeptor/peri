import { cn } from "@/lib/utils"

function hueFor(model: string) {
  return [...model].reduce(
    (hash, char) => (hash * 31 + char.charCodeAt(0)) % 360,
    0
  )
}

export function analystLabel(model: string) {
  return model || "muse-legacy"
}

export function analystShortName(model: string) {
  return analystLabel(model).replace(/-free$/, "")
}

export function AnalystChip({
  analyst,
  className,
  full = false,
}: {
  analyst: string
  className?: string
  full?: boolean
}) {
  const label = analystLabel(analyst)
  const display = full ? label : analystShortName(analyst)
  const legacy = !analyst

  return (
    <span
      className={cn(
        "inline-flex max-w-full items-center gap-1 rounded-full px-2 py-0.5 font-mono text-[10px] font-medium",
        legacy ? "bg-surface-2 text-muted-foreground" : "border",
        className
      )}
      style={
        legacy
          ? undefined
          : {
              backgroundColor: `hsl(${hueFor(analyst)} 65% 48% / 0.12)`,
              borderColor: `hsl(${hueFor(analyst)} 65% 56% / 0.35)`,
              color: `hsl(${hueFor(analyst)} 65% 72%)`,
            }
      }
      title={label}
    >
      <span className="truncate">{display}</span>
    </span>
  )
}
