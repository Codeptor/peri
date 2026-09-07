// Mirrors kestreld/kestreld.toml — update here if the toml changes.
export const BANKROLL = 1000.0; // [engine] bankroll
export const KILL_SWITCH_PCT = 12.0; // [risk] kill_switch_pct — equity drawdown halt
export const DAILY_TRADE_CAP = 20; // [risk] daily_cap
export const MAX_CONCURRENT = 5; // [risk] max_concurrent
export const MARGIN_MAX = 50.0; // [sizing] margin_max per position

/** Fraction (0..1) of the kill-switch drawdown budget consumed at `equity`. */
export function killBudgetUsed(equity: number): number {
  const dd = Math.max(0, BANKROLL - equity);
  return Math.min(1, dd / (BANKROLL * (KILL_SWITCH_PCT / 100)));
}
