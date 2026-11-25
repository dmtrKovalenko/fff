import { sliceColumns, visibleWidth, wrap } from "@oh-my-pi/pi-tui/text/width";
import { createFffExtension } from "./core";
import type { TextComponent } from "./tui";

export { fffFileAnnotation } from "./core";
export { SCAN_TIMEOUT_MS } from "./sdk";

class Text implements TextComponent {
  constructor(private text: string) {}

  setText(text: string): void {
    this.text = text;
  }

  render(width: number): string[] {
    return this.text.trim() ? wrap(this.text, width) : [];
  }

  invalidate(): void {}
}

export default createFffExtension({
  Text,
  sliceByColumn: (text, start, length) => sliceColumns(text, start, start + length),
  visibleWidth,
});
