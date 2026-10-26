import type { Input } from "../types";

/**
 * The inputs the run form shows: every input that is not advanced, plus the
 * advanced ones once "More options" is open. `hidden` counts what is left out.
 */
export function formInputs(inputs: Input[], expanded: boolean): { shown: Input[]; hidden: number } {
  const shown = inputs.filter((i) => expanded || !i.advanced);
  return { shown, hidden: inputs.length - shown.length };
}
