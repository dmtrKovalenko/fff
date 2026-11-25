export interface Component {
  render(width: number): string[];
  invalidate(): void;
}

export interface TextComponent extends Component {
  setText(text: string): void;
}

export interface AutocompleteItem {
  value: string;
  label: string;
  description?: string;
}

export interface AutocompleteSuggestions {
  items: AutocompleteItem[];
  prefix: string;
}

export interface AutocompleteProvider {
  getSuggestions(
    lines: string[],
    cursorLine: number,
    cursorCol: number,
    options?: { signal: AbortSignal; force?: boolean } | AbortSignal,
  ): Promise<AutocompleteSuggestions | null>;
  applyCompletion(
    lines: string[],
    cursorLine: number,
    cursorCol: number,
    item: AutocompleteItem,
    prefix: string,
  ): { lines: string[]; cursorLine: number; cursorCol: number };
  shouldTriggerFileCompletion?(
    lines: string[],
    cursorLine: number,
    cursorCol: number,
  ): boolean;
}

export interface TuiAdapter {
  Text: new (text: string, paddingX: number, paddingY: number) => TextComponent;
  sliceByColumn(text: string, start: number, length: number, strict?: boolean): string;
  visibleWidth(text: string): number;
}
