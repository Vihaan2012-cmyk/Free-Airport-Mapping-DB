// SPDX-License-Identifier: GPL-3.0
//
// The taxi route the bridge keeps, onto the OANS: asked for every two seconds and drawn
// (in magenta, on the BTV layer) when it is for the airport the OANS is showing. The
// OANS toolbar window sets it; the A320 OANS on the ND shows it too.

import { EventBus } from '@microsoft/msfs-sdk';

const BRIDGE = 'http://127.0.0.1:8770';
const POLL_MS = 2000;

export interface TaxiRoute {
  icao: string;
  clearance: string;
  legs: string[];
  to: string;
  length_m: number;
  /** In the airport's own metres, as its map is drawn. */
  points: [number, number][];
  serial: number;
}

export class TaxiRouteFeed {
  /** What is drawn now: the route's serial and the airport, or none. */
  private shown = '';

  private busy = false;

  private next = 0;

  private last: TaxiRoute | null = null;

  constructor(
    private readonly bus: EventBus,
    private readonly airport: () => string | null,
    private readonly onRoute?: (route: TaxiRoute | null) => void,
  ) {}

  /** Call every frame: asks the bridge now and then, and redraws when the airport changes. */
  public update(now = Date.now()): void {
    this.show(this.last);
    if (this.busy || now < this.next) {
      return;
    }
    this.next = now + POLL_MS;
    this.busy = true;
    fetch(`${BRIDGE}/amdb/taxi-route/current`)
      .then((r) => r.json())
      .then(
        (route: TaxiRoute | null) => {
          this.busy = false;
          this.last = route && Array.isArray(route.points) ? route : null;
          this.show(this.last);
        },
        () => {
          this.busy = false;
        },
      );
  }

  /** Ask the bridge again on the next update, as after setting or clearing the route. */
  public refresh(): void {
    this.next = 0;
  }

  private show(route: TaxiRoute | null): void {
    const icao = this.airport();
    const drawn = route && icao && route.icao.toUpperCase() === icao.toUpperCase() ? route : null;
    const key = drawn ? `${drawn.serial}|${icao}` : `none|${icao}`;
    if (key === this.shown) {
      return;
    }
    this.shown = key;
    this.bus.getPublisher<any>().pub('amdb_taxi_route', drawn ? drawn.points : null, false, true);
    if (this.onRoute) {
      this.onRoute(drawn);
    }
  }

  /** Route a clearance from a position; the bridge keeps it as the current route. */
  public static set(icao: string, lat: number, lon: number, clearance: string): Promise<TaxiRoute | { error: string }> {
    const q = `icao=${encodeURIComponent(icao)}&lat=${lat}&lon=${lon}&clearance=${encodeURIComponent(clearance)}`;
    return fetch(`${BRIDGE}/amdb/taxi-route?${q}`).then(
      (r) => r.json(),
      () => ({ error: 'AMDB Bridge is not running' }),
    );
  }

  public static clear(): Promise<unknown> {
    return fetch(`${BRIDGE}/amdb/taxi-route/clear`).then(
      (r) => r.json(),
      () => null,
    );
  }
}
