// SPDX-License-Identifier: GPL-3.0
//
// Set at build time (build.mjs, from the environment).

/** Whether AMDB Bridge may be asked anything: false for an aircraft that ships without it. */
declare const AMDB_BRIDGE: boolean;
/** Where the airports exported for the web are hosted; empty for none. */
declare const AMDB_DATA_URL: string;
/** Where the airports bundled with the aircraft are (its html_ui path); empty for none. */
declare const AMDB_BUNDLE_URL: string;
