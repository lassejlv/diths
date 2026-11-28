/**
 * Error codes, as in the dith CLI's JSON envelope. `INVALID_ARGUMENT` covers out-of-range values,
 * unknown options and inputs that cannot be read.
 */
export type DithErrorCode =
  | "INVALID_ARGUMENT"
  | "INVALID_RECIPE_JSON"
  | "UNKNOWN_PRESET"
  | "UNKNOWN_PALETTE"
  | "PALETTE_TOO_FEW_COLORS"
  | "INVALID_HEX_COLOR"
  | "PALETTE_IO_ERROR"
  | "INVALID_ASSET"
  | "RUNTIME_ERROR"
  | (string & {});

/**
 * Every failure is a `DithError` with the fields of the CLI's `--json` error envelope:
 * a machine `code`, a human `message`, a recovery `suggestion` and, when the set is known,
 * `validValues`. `toJSON()` returns the envelope itself.
 */
export class DithError extends Error {
  override readonly name = "DithError";
  readonly code: DithErrorCode;
  readonly suggestion: string;
  readonly validValues?: string[];

  constructor(code: DithErrorCode, message: string, suggestion: string, validValues?: string[], options?: { cause?: unknown }) {
    super(message, options);
    this.code = code;
    this.suggestion = suggestion;
    if (validValues) this.validValues = validValues;
  }

  /** The CLI's error envelope: `{ status: "error", code, message, suggestion, valid_values? }`. */
  toJSON(): { status: "error"; code: string; message: string; suggestion: string; valid_values?: string[] } {
    return {
      status: "error",
      code: this.code,
      message: this.message,
      suggestion: this.suggestion,
      ...(this.validValues ? { valid_values: this.validValues } : {}),
    };
  }
}

/** Turn anything thrown inside the module into a `DithError`. */
export function toDithError(err: unknown): DithError {
  if (err instanceof DithError) return err;
  const e = err as { name?: string; code?: string; message?: string; suggestion?: string; validValues?: string[] } | undefined;
  if (e?.name === "DithError" && e.code) return new DithError(e.code, e.message ?? "", e.suggestion ?? "", e.validValues);
  return new DithError(
    "RUNTIME_ERROR",
    err instanceof Error ? err.message : String(err),
    "Check the input and options; the same recipe should fail the same way in the dith CLI.",
    undefined,
    { cause: err },
  );
}

/** An `INVALID_ARGUMENT` error, worded like the CLI's. */
export function invalid(field: string, reason: string, validValues?: readonly string[], suggestion?: string): DithError {
  return new DithError(
    "INVALID_ARGUMENT",
    `invalid argument '${field}': ${reason}`,
    suggestion ?? (validValues ? `Use one of: ${validValues.join(", ")}.` : "Run `dith capabilities` to inspect valid parameter names and value ranges."),
    validValues ? [...validValues] : undefined,
  );
}
