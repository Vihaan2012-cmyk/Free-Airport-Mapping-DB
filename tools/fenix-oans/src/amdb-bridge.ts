// SPDX-License-Identifier: GPL-3.0
//
// Stands in for FlyByWire's shared/src/navigraph/amdb.ts in the Fenix build (see the
// plugin in build.mjs): the same two queries, asked of the local amdb-bridge over plain
// HTTP rather than of Navigraph's API through a signed-in Navigraph session.

import type { AmdbAirportSearchResponse, AmdbResponse, FeatureTypeString } from '@flybywiresim/fbw-sdk';

const BRIDGE = 'http://127.0.0.1:8770/v1';

async function get<T>(query: string): Promise<T> {
  const response = await fetch(`${BRIDGE}/${query}`, { headers: { Accept: 'application/json' } });
  if (!response.ok) {
    throw new Error(`amdb-bridge answered ${response.status} for /v1/${query}`);
  }
  return response.json();
}

export async function searchAmdbAirports(queryString: string): Promise<AmdbAirportSearchResponse> {
  return get(`search?q=${encodeURIComponent(queryString)}`);
}

export async function getAmdbData(
  icao: string,
  includeFeatureTypes?: FeatureTypeString[],
  excludeFeatureTypes?: FeatureTypeString[],
  projection = 'NAVIGRAPH:ARP_AZEQ',
): Promise<AmdbResponse> {
  const exclude = excludeFeatureTypes ? excludeFeatureTypes.join(',') : '';
  const include = includeFeatureTypes ? includeFeatureTypes.join(',') : '';
  return get(`${icao}?projection=${projection}&format=geojson&exclude=${exclude}&include=${include}`);
}
