// Bundled inside the omp binary; not installable as a dev dependency here.
declare module "@oh-my-pi/pi-tui/text/width" {
  export function visibleWidth(text: string): number;
  export function sliceColumns(text: string, start: number, end: number): string;
  export function wrap(text: string, width: number): string[];
}
