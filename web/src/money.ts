// Integer micro-USD formatting, byte-identical to tare-core's MicroUsd::to_dollar_string.
// All values are within JS safe-integer range.

export function toDollarString(micros: number): string {
  const neg = micros < 0;
  const abs = Math.abs(micros);
  const dollars = Math.floor(abs / 1_000_000);
  const frac = abs % 1_000_000;
  return (neg ? "-" : "") + "$" + dollars + "." + String(frac).padStart(6, "0");
}
