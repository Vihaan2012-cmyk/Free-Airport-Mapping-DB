// SPDX-License-Identifier: GPL-3.0
//
// Stands in for FlyByWire's shared/src/navigraph/amdb.ts in this build (see the plugin in
// build.mjs): the same two queries, answered by the first of these that can:
//
// 1. the local amdb-bridge, when the build may ask it (AMDB_BRIDGE) and it is running;
// 2. the airports exported for the web (`amdb-bridge export-web`) at AMDB_DATA_URL, the
//    newest there are, and all an aircraft on a console can reach;
// 3. the same airports packed into the aircraft (`amdb-bridge bundle-web`) at
//    AMDB_BUNDLE_URL, for when the web cannot be reached: offline, or requests blocked.
//
// A build with neither address asks the bridge alone, as before.

import type { AmdbAirportSearchResponse, AmdbResponse, FeatureTypeString } from '@flybywiresim/fbw-sdk';
import { inflateSync, strFromU8 } from 'fflate';

const BRIDGE = 'http://127.0.0.1:8770/v1';
// Set at build time (build.mjs), and changeable by the aircraft at run time
// (`configureAmdb`): an aircraft that takes this file as it is says where its data is.
let useBridge = AMDB_BRIDGE;
let HOSTED = AMDB_DATA_URL.replace(/\/+$/, '');
let BUNDLE = AMDB_BUNDLE_URL.replace(/\/+$/, '');
/** The sphere the airports' metre grid is drawn on. */
const R = 6371008.8;
/** How long the web host is given to answer before the bundle is used instead. */
const HOSTED_WAIT_MS = 5000;

/**
 * Where the airports come from, set before the first airport is asked for. `dataUrl` is the
 * web host (`export-web`'s files, e.g. the Hugging Face dataset's `.../resolve/main`),
 * `bundleUrl` the airports packed into the aircraft (`bundle-web`'s folder, as an html_ui
 * path), `bridge` whether AMDB Bridge on this computer may be asked. An empty address is
 * none; what is left out stays as it was built.
 */
export function configureAmdb(options: { dataUrl?: string; bundleUrl?: string; bridge?: boolean }): void {
  if (options.dataUrl !== undefined) {
    HOSTED = options.dataUrl.replace(/\/+$/, '');
    hostedReachable = HOSTED !== '';
  }
  if (options.bundleUrl !== undefined) {
    BUNDLE = options.bundleUrl.replace(/\/+$/, '');
    bundleIndex = undefined;
    packs.clear();
  }
  if (options.bridge !== undefined) {
    useBridge = options.bridge;
    bridgeAnswers = undefined;
  }
  airports.clear();
}

async function get<T>(query: string): Promise<T> {
  const response = await fetch(`${BRIDGE}/${query}`, { headers: { Accept: 'application/json' } });
  if (!response.ok) {
    throw new Error(`amdb-bridge answered ${response.status} for /v1/${query}`);
  }
  return response.json();
}

/** Whether the bridge is there to ask, found out once. */
let bridgeAnswers: Promise<boolean> | undefined;
function bridgeUp(): Promise<boolean> {
  if (!useBridge) {
    return Promise.resolve(false);
  }
  if (!HOSTED && !BUNDLE) {
    return Promise.resolve(true);
  }
  if (!bridgeAnswers) {
    bridgeAnswers = new Promise((resolve) => {
      const timer = setTimeout(() => resolve(false), 1500);
      fetch(`${BRIDGE}/cycle`).then(
        (r) => {
          clearTimeout(timer);
          resolve(r.ok);
        },
        () => {
          clearTimeout(timer);
          resolve(false);
        },
      );
    });
  }
  return bridgeAnswers;
}

/** Cleared the first time the web host cannot be reached: the bundle answers from then on. */
let hostedReachable = HOSTED !== '';

/** Not there: the host answered, and said so. */
class Missing extends Error {}

function hosted<T>(path: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${HOSTED} did not answer in time`)), HOSTED_WAIT_MS);
    fetch(`${HOSTED}/${path}`).then(
      (r) => {
        clearTimeout(timer);
        if (!r.ok) {
          reject(new Missing(`${HOSTED}/${path} answered ${r.status}`));
        } else {
          r.json().then(resolve, reject);
        }
      },
      (e) => {
        clearTimeout(timer);
        reject(e);
      },
    );
  });
}

/** The bundle's index, `{key: [pack, offset, length]}`, and the last packs read. */
let bundleIndex: Promise<Record<string, [number, number, number]>> | undefined;
const packs = new Map<number, Promise<Uint8Array>>();

async function bundled<T>(key: string): Promise<T> {
  if (!BUNDLE) {
    throw new Missing(`${key}: no airports are bundled with this aircraft`);
  }
  if (!bundleIndex) {
    bundleIndex = fetch(`${BUNDLE}/index.json`)
      .then((r) => r.json())
      .then((j) => j.files);
  }
  const at = (await bundleIndex)[key];
  if (!at) {
    throw new Missing(`${key} is not bundled with this aircraft`);
  }
  const [n, offset, length] = at;
  let pack = packs.get(n);
  if (!pack) {
    if (packs.size >= 2) {
      packs.delete(packs.keys().next().value as number);
    }
    pack = fetch(`${BUNDLE}/p${String(n).padStart(3, '0')}.bin`)
      .then((r) => r.arrayBuffer())
      .then((b) => new Uint8Array(b));
    pack.catch(() => packs.delete(n));
    packs.set(n, pack);
  }
  return JSON.parse(strFromU8(inflateSync((await pack).subarray(offset, offset + length))));
}

/** A file from the web host, or from the bundle when the host cannot be reached. */
async function read<T>(hostedPath: string, bundleKey: string): Promise<T> {
  if (hostedReachable) {
    try {
      return await hosted<T>(hostedPath);
    } catch (e) {
      // The host answered that it has no such file: so does the bundle, which is the same data.
      if (e instanceof Missing || !BUNDLE) {
        throw e;
      }
      hostedReachable = false;
      console.warn(`[AMDB] ${e}; reading the airports bundled with the aircraft from now on`);
    }
  }
  return bundled<T>(bundleKey);
}

interface AirportFile {
  icao: string;
  /** The reference point the metre grid is centred on: [lat, lon]. */
  arp: [number, number];
  layers: Record<string, any>;
}

/** The last few airports read, kept: the map asks for one airport several times over. */
const airports = new Map<string, Promise<AirportFile>>();
function airportFile(icao: string): Promise<AirportFile> {
  const code = icao.toUpperCase();
  let file = airports.get(code);
  if (!file) {
    if (airports.size >= 4) {
      airports.delete(airports.keys().next().value as string);
    }
    file = read<AirportFile>(`v1/${code.slice(0, 2)}/${code}.json`, code);
    // Not kept when it failed, so that it is asked for again.
    file.catch(() => airports.delete(code));
    airports.set(code, file);
  }
  return file;
}

/** Navigraph spells one layer its own way. */
const ALIAS: Record<string, string> = { touchdownliftofarea: 'touchdownliftoffarea' };

/** Metres east and north of the reference point to [lon, lat]. */
function toLonLat(arp: [number, number], x: number, y: number): [number, number] {
  const lat0 = (arp[0] * Math.PI) / 180;
  const rho = Math.hypot(x, y);
  if (rho < 1e-9) {
    return [arp[1], arp[0]];
  }
  const c = rho / R;
  const sinc = Math.sin(c);
  const cosc = Math.cos(c);
  const lat = Math.asin(Math.max(-1, Math.min(1, cosc * Math.sin(lat0) + (y * sinc * Math.cos(lat0)) / rho)));
  const lon = (arp[1] * Math.PI) / 180 + Math.atan2(x * sinc, rho * Math.cos(lat0) * cosc - y * Math.sin(lat0) * sinc);
  return [(lon * 180) / Math.PI, (lat * 180) / Math.PI];
}

function mapCoordinates(c: any, arp: [number, number]): any {
  return typeof c[0] === 'number' ? toLonLat(arp, c[0], c[1]) : c.map((inner: any) => mapCoordinates(inner, arp));
}

/** A layer in latitude and longitude, as the API answers EPSG:4326. */
function toWgs84(collection: any, arp: [number, number]): any {
  const point = (g: any) => (g && g.coordinates ? { ...g, coordinates: mapCoordinates(g.coordinates, arp) } : g);
  return {
    ...collection,
    features: collection.features.map((f: any) => {
      const properties = { ...f.properties };
      for (const key of ['centroid', 'midpoint']) {
        if (properties[key]) {
          properties[key] = point(properties[key]);
        }
      }
      return { ...f, geometry: point(f.geometry), properties };
    }),
  };
}

export async function searchAmdbAirports(queryString: string): Promise<AmdbAirportSearchResponse> {
  if (await bridgeUp()) {
    return get(`search?q=${encodeURIComponent(queryString)}`);
  }
  const q = queryString.trim().toUpperCase();
  if (!q) {
    return read('v1/search.json', 'search');
  }
  if (!/^[0-9A-Z]/.test(q)) {
    return [] as any;
  }
  const rows = await read<any[]>(`v1/search/${q[0]}.json`, `search/${q[0]}`).catch(() => [] as any[]);
  return rows.filter((r) => r.idarpt.startsWith(q) || (r.iata ?? '').toUpperCase().startsWith(q) || (r.name ?? '').toUpperCase().startsWith(q)) as any;
}

export async function getAmdbData(
  icao: string,
  includeFeatureTypes?: FeatureTypeString[],
  excludeFeatureTypes?: FeatureTypeString[],
  projection = 'NAVIGRAPH:ARP_AZEQ',
): Promise<AmdbResponse> {
  if (await bridgeUp()) {
    const exclude = excludeFeatureTypes ? excludeFeatureTypes.join(',') : '';
    const include = includeFeatureTypes ? includeFeatureTypes.join(',') : '';
    return get(`${icao}?projection=${projection}&format=geojson&exclude=${exclude}&include=${include}`);
  }
  const file = await airportFile(icao);
  const wgs84 = projection.toUpperCase() === 'EPSG:4326';
  const canonical = (n: string) => ALIAS[n.toLowerCase()] ?? n.toLowerCase();
  const names = includeFeatureTypes && includeFeatureTypes.length ? includeFeatureTypes.map((n) => n.toLowerCase()) : Object.keys(file.layers);
  const skip = new Set((excludeFeatureTypes ?? []).map(canonical));
  const out: Record<string, any> = {};
  for (const name of names) {
    const key = canonical(name);
    const layer = file.layers[key];
    if (layer && !skip.has(key)) {
      out[name] = wgs84 ? toWgs84(layer, file.arp) : layer;
    }
  }
  return out as AmdbResponse;
}
