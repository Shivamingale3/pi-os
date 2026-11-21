// Accepts: "42", "-3", "0.29", " 15750520\n"
// Rejects: "", "abc", "0x1A", "1e3", "Infinity", "NaN", "1.", ".5", "1,000"
const DECIMAL = /^-?\d+(\.\d+)?$/;

export function parseNum(s: string, what: string): number {
  const t = s.trim();
  if (!DECIMAL.test(t)) {
    throw new Error(`bad number for ${what}: ${JSON.stringify(s)}`);
  }
  const n = Number(t);
  if (!Number.isFinite(n)) {
    throw new Error(`number too large for ${what}: ${JSON.stringify(s)}`);
  }
  return n;
}
