// SPDX-License-Identifier: GPL-3.0
//
// The Airport Map toolbar window: FlyByWire's A380X OANS (see OansDisplay) in a window of
// its own, for any aircraft, with its own mode and range, and a taxi clearance box. Type
// the taxiways and where to -- "A B K 31L", "B K STAND 73" -- and AMDB Bridge finds the way
// from the aircraft along them; it is drawn in magenta here, and on the A320 OANS.
//
// The window keeps an event bus of its own, not joined to other instruments', so the
// A320 OANS on the ND keeps its own airport, exits and BTV while this one is open.

import { Clock, FSComponent, InstrumentBackplane, Subject } from '@microsoft/msfs-sdk';
import { ArincEventBus, BtvSimvarPublisher, EfisNdMode, FmsOansSimvarPublisher, OansControlEvents } from '@flybywiresim/fbw-sdk';
import { a380EfisZoomRangeSettings } from '@flybywiresim/oanc';
import { RopRowOansPublisher } from '@flybywiresim/msfs-avionics-common';
import { ResetPanelSimvarPublisher } from '@a380x/MsfsAvionicsCommon/providers/ResetPanelPublisher';
import { AircraftPublisher } from './AircraftPublisher';
import { OansDisplay, reportOnce } from './OansDisplay';
import { TaxiRoute, TaxiRouteFeed } from './TaxiRouteFeed';

import './toolbar.scss';

declare class TemplateElement extends HTMLElement {
  connectedCallback(): void;
}
declare function checkAutoload(): void;

const MODES: [string, EfisNdMode][] = [
  ['ARC', EfisNdMode.ARC],
  ['ROSE', EfisNdMode.ROSE_NAV],
  ['PLAN', EfisNdMode.PLAN],
];
/** 0.5 NM, of 0.2, 0.5, 1, 2 and 5. */
const DEFAULT_RANGE = 1;
const INPUT_ID = 'AmdbTaxiClearance';

function describe(route: TaxiRoute): string {
  const via = route.legs.length ? route.legs.join(' ') : 'direct';
  return `${via} TO ${route.to} · ${Math.round(route.length_m)} M`;
}

class AirportMap {
  /** Not synced with other instruments' buses (see above). */
  private readonly bus = new (ArincEventBus as any)(() => ({ sendSyncedEvent: () => undefined }), false) as ArincEventBus;

  private readonly backplane = new InstrumentBackplane();

  private readonly display = new OansDisplay(this.bus);

  private readonly taxiRoute = new TaxiRouteFeed(this.bus, () => this.display.airport(), (r) => this.onRoute(r));

  private readonly mode = Subject.create(EfisNdMode.ARC);

  private readonly range = Subject.create(DEFAULT_RANGE);

  private readonly status = Subject.create('');

  private readonly statusKind = Subject.create<'' | 'route' | 'warn'>('');

  private readonly screenRef = FSComponent.createRef<HTMLDivElement>();

  private readonly contentRef = FSComponent.createRef<HTMLDivElement>();

  private readonly modesRef = FSComponent.createRef<HTMLDivElement>();

  private readonly zoomOutRef = FSComponent.createRef<HTMLDivElement>();

  private readonly zoomInRef = FSComponent.createRef<HTMLDivElement>();

  private readonly mapDataRef = FSComponent.createRef<HTMLDivElement>();

  private readonly inputRef = FSComponent.createRef<HTMLInputElement>();

  private readonly routeRef = FSComponent.createRef<HTMLDivElement>();

  private readonly clearRef = FSComponent.createRef<HTMLDivElement>();

  private active = false;

  private fitted = '';

  constructor(container: HTMLElement) {
    this.backplane.addInstrument('aircraft', new AircraftPublisher(this.bus));
    // The control panel's airport auto-load runs on the clock's `realTime`, as on the A380X ND.
    this.backplane.addInstrument('clock', new Clock(this.bus));
    this.backplane.addPublisher('fms-oans', new FmsOansSimvarPublisher(this.bus));
    this.backplane.addPublisher('rop-row-oans', new RopRowOansPublisher(this.bus));
    this.backplane.addPublisher('btv', new BtvSimvarPublisher(this.bus));
    this.backplane.addPublisher('resetPanel', new ResetPanelSimvarPublisher(this.bus));
    this.backplane.init();

    FSComponent.render(this.layout(), container);
    this.display.render(this.contentRef.instance);
    this.wire();

    const pub = this.bus.getPublisher<OansControlEvents & { ndMode: EfisNdMode; oansRange: number }>();
    this.mode.sub((m) => pub.pub('ndMode', m, false, true), true);
    this.range.sub((r) => pub.pub('oansRange', r, false, true), true);
  }

  private layout() {
    const button = (selected: boolean) => (selected ? 'amdb-map-button selected' : 'amdb-map-button');
    return (
      <div class="amdb-map">
        <div ref={this.screenRef} class="amdb-map-screen">
          <div ref={this.contentRef} class="amdb-map-content" />
        </div>
        <div class="amdb-map-bar">
          <div class="amdb-map-row">
            <div ref={this.modesRef} class="amdb-map-group">
              {MODES.map(([label, mode]) => (
                <div class={this.mode.map((m) => button(m === mode))} data-mode={String(mode)}>
                  {label}
                </div>
              ))}
            </div>
            <div class="amdb-map-group">
              <div ref={this.zoomOutRef} class="amdb-map-button">
                {'−'}
              </div>
              <div class="amdb-map-range">{this.range.map((i) => `${a380EfisZoomRangeSettings[i]} NM`)}</div>
              <div ref={this.zoomInRef} class="amdb-map-button">
                +
              </div>
            </div>
            <div ref={this.mapDataRef} class={this.display.controlPanelVisible.map(button)}>
              MAP DATA
            </div>
          </div>
          <div class="amdb-map-row">
            <span class="amdb-map-label">TAXI</span>
            <input ref={this.inputRef} id={INPUT_ID} class="amdb-map-clearance" type="text" placeholder="A B K 31L" />
            <div ref={this.routeRef} class="amdb-map-button">
              ROUTE
            </div>
            <div ref={this.clearRef} class="amdb-map-button">
              CLEAR
            </div>
          </div>
          <div class={this.statusKind.map((k) => `amdb-map-status ${k}`)}>{this.status}</div>
        </div>
      </div>
    );
  }

  private wire(): void {
    this.modesRef.instance.addEventListener('click', (e) => {
      const target = (e.target as HTMLElement).closest('[data-mode]');
      if (target) {
        this.mode.set(Number(target.getAttribute('data-mode')) as EfisNdMode);
      }
    });
    this.zoomOutRef.instance.addEventListener('click', () => this.range.set(Math.min(a380EfisZoomRangeSettings.length - 1, this.range.get() + 1)));
    this.zoomInRef.instance.addEventListener('click', () => this.range.set(Math.max(0, this.range.get() - 1)));
    this.mapDataRef.instance.addEventListener('click', () => this.display.controlPanelVisible.set(!this.display.controlPanelVisible.get()));
    this.routeRef.instance.addEventListener('click', () => this.submit());
    this.clearRef.instance.addEventListener('click', () => {
      this.inputRef.instance.value = '';
      TaxiRouteFeed.clear().then(() => this.taxiRoute.refresh());
    });

    // Typing goes to the window, not to the simulator's key bindings, while the box has focus.
    const input = this.inputRef.instance;
    input.addEventListener('focus', () => Coherent.trigger('FOCUS_INPUT_FIELD', INPUT_ID, '', '', '', false));
    input.addEventListener('blur', () => Coherent.trigger('UNFOCUS_INPUT_FIELD', INPUT_ID));
    input.addEventListener('keydown', (e) => {
      if (e.keyCode === 13) {
        this.submit();
        input.blur();
      } else if (e.keyCode === 27) {
        input.blur();
      }
    });
  }

  private say(text: string, kind: '' | 'route' | 'warn'): void {
    this.status.set(text);
    this.statusKind.set(kind);
  }

  private submit(): void {
    const clearance = this.inputRef.instance.value.trim();
    if (!clearance) {
      this.say('Type the taxiways, then a runway or a stand: A B K 31L', 'warn');
      return;
    }
    const icao = this.display.airport();
    if (!icao) {
      this.say('No airport on the map yet: pick one in MAP DATA', 'warn');
      return;
    }
    const lat = SimVar.GetSimVarValue('PLANE LATITUDE', 'degree latitude');
    const lon = SimVar.GetSimVarValue('PLANE LONGITUDE', 'degree longitude');
    this.say(`Routing ${clearance.toUpperCase()} at ${icao}…`, '');
    reportOnce('toolbar-route-asked');
    TaxiRouteFeed.set(icao, lat, lon, clearance).then((r) => {
      if ('error' in r) {
        this.say(r.error, 'warn');
      } else {
        this.say(describe(r), 'route');
        this.taxiRoute.refresh();
      }
    });
  }

  private onRoute(route: TaxiRoute | null): void {
    if (route) {
      this.say(describe(route), 'route');
    } else if (this.statusKind.get() === 'route') {
      this.say('', '');
    }
  }

  public setActive(active: boolean): void {
    this.active = active;
    this.bus.getPublisher<OansControlEvents>().pub('nd_show_oans', { side: 'L', show: active }, false, true);
    if (active) {
      this.fitted = '';
      this.taxiRoute.refresh();
      requestAnimationFrame(() => this.frame());
    } else {
      this.inputRef.instance.blur();
    }
  }

  private frame(): void {
    if (!this.active) {
      return;
    }
    this.fit();
    this.taxiRoute.update();
    this.backplane.onUpdate();
    this.display.update();
    requestAnimationFrame(() => this.frame());
  }

  /** The display is laid out at 768 x 768: scaled to the largest square the window has room for. */
  private fit(): void {
    const screen = this.screenRef.instance;
    const w = screen.clientWidth;
    const h = screen.clientHeight;
    const key = `${w}x${h}`;
    if (key === this.fitted || w <= 0 || h <= 0) {
      return;
    }
    this.fitted = key;
    const k = Math.min(w, h) / 768;
    const content = this.contentRef.instance;
    content.style.transformOrigin = '0 0';
    content.style.transform = `translate(${(w - 768 * k) / 2}px, ${(h - 768 * k) / 2}px) scale(${k})`;
    reportOnce(`toolbar-window-${w}x${h}`);
  }
}

class AmdbOansMapPanel extends TemplateElement {
  private map: AirportMap | null = null;

  public connectedCallback(): void {
    super.connectedCallback();
    const frame = this.querySelector('ingame-ui');
    const container = this.querySelector('#AmdbOansMap') as HTMLElement | null;
    if (!frame || !container) {
      reportOnce('toolbar-page-incomplete');
      return;
    }
    this.map = new AirportMap(container);
    frame.addEventListener('panelActive', () => this.map?.setActive(true));
    frame.addEventListener('panelInactive', () => this.map?.setActive(false));
    reportOnce('toolbar-ready');
  }
}

window.customElements.define('amdb-oans-map', AmdbOansMapPanel);
checkAutoload();
