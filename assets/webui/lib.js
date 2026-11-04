// One place for the vendored libraries, since a CSP without inline scripts rules out an import map.
import { h, render, Fragment } from "./vendor/preact.js";
import { useState, useEffect, useRef, useLayoutEffect, useMemo, useCallback } from "./vendor/preact-hooks.js";
import htm from "./vendor/htm.js";
import { marked } from "./vendor/marked.js";
import DOMPurify from "./vendor/dompurify.js";

export const html = htm.bind(h);
export { h, render, Fragment, useState, useEffect, useRef, useLayoutEffect, useMemo, useCallback, marked, DOMPurify };
