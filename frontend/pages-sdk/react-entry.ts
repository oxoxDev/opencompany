// Built standalone to `dist/pages-sdk/react.mjs` — the one ESM module a
// page's served import map resolves BOTH bare specifiers "react" and
// "react-dom/client" to (docs/spec/runtime/pages.md §5). Because both
// specifiers point at this same file, its exports have to satisfy both
// import shapes at once: `import React from "react"` (a default export) and
// `import { createRoot } from "react-dom/client"` (a named export) both
// resolve here, which is why this module re-exports React's own named
// exports, hands back React itself as the default export, and adds
// react-dom/client's exports alongside them.
//
// Bundles this app's own pinned React/ReactDOM versions rather than the
// console's own chunk, so a page's module graph is self-contained and never
// depends on the console build's internal chunk hashes.
import * as React from "react";
import { createRoot, hydrateRoot } from "react-dom/client";
import { jsx, jsxs } from "react/jsx-runtime";

// `export * from "react"` doesn't typecheck here: `@types/react` declares
// itself with `export =`, and TS refuses to re-export `*` from a module
// shaped that way. Destructuring the namespace import instead re-exports the
// same runtime values (React's CJS module carries these as plain
// properties) without hitting that restriction.
export default React;
export const {
  Children,
  Component,
  Fragment,
  Profiler,
  PureComponent,
  StrictMode,
  Suspense,
  cloneElement,
  createContext,
  createElement,
  createRef,
  forwardRef,
  isValidElement,
  lazy,
  memo,
  startTransition,
  useCallback,
  useContext,
  useDebugValue,
  useDeferredValue,
  useEffect,
  useId,
  useImperativeHandle,
  useInsertionEffect,
  useLayoutEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
  useSyncExternalStore,
  useTransition,
  version,
} = React;
export { jsx, jsxs };
export { createRoot, hydrateRoot };
