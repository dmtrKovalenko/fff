import { sliceByColumn, Text, visibleWidth } from "@earendil-works/pi-tui";
import { createFffExtension } from "./core";

export { fffFileAnnotation } from "./core";
export { SCAN_TIMEOUT_MS } from "./sdk";

export default createFffExtension({ Text, sliceByColumn, visibleWidth });
