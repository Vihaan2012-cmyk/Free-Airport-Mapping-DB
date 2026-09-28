// SPDX-License-Identifier: GPL-3.0
//
// FlyByWire's A380X OANS (https://github.com/flybywiresim/aircraft, GPL-3.0) running as an
// overlay on a Fenix A320 ND: the captain's, or the first officer's when panel.cfg loads
// this gauge with `?Index=2`. The OANS, its control panel, the context menu and the erase
// dialogs are FlyByWire's own components (see OansDisplay); what the A380X's systems would
// feed them comes from FenixOansPublisher.
//
// There is one BTV: the captain's side brakes (FenixBtv), and tells the first officer's
// side what it is doing so both NDs show it. Either side's exit pick reaches it, as
// FlyByWire's OANS shares a picked exit between its two sides.

import { Clock, ConsumerSubject, FSComponent, InstrumentBackplane, Subscribable } from '@microsoft/msfs-sdk';
import { ArincEventBus, BtvSimvarPublisher, FmsOansData, FmsOansSimvarPublisher } from '@flybywiresim/fbw-sdk';
import { RopRowOansPublisher } from '@flybywiresim/msfs-avionics-common';
import { ResetPanelSimvarPublisher } from '@a380x/MsfsAvionicsCommon/providers/ResetPanelPublisher';
import { FenixOansPublisher } from './FenixOansPublisher';
import { BtvState, BtvStatus, FenixBtv } from './FenixBtv';
import { OansDisplay, reportOnce } from './OansDisplay';
import { TaxiRouteFeed } from './TaxiRouteFeed';

function btvText(s: BtvStatus): string {
  if (s.state === BtvState.Off || s.state === BtvState.LettingGo || !s.exit) {
    return '';
  }
  if (s.missed) {
    return `BTV ${s.exit} EXIT MISSED`;
  }
  return s.metres === null ? `BTV ${s.exit}` : `BTV ${s.exit} ${s.metres}M`;
}

/** Armed in cyan, as FBW's selected exit; braking in green; a missed exit in amber. */
function btvClass(s: BtvStatus): string {
  const colour = s.missed ? 'Amber' : s.state === BtvState.Armed ? 'Cyan' : 'Green';
  return `${colour} FontIntermediate MiddleAlign`;
}

/** What the captain's side tells the first officer's side about BTV. */
interface BtvShared {
  amdb_btv_status: BtvStatus;
}

const BTV_IDLE: BtvStatus = { state: BtvState.Off, exit: null, metres: null, missed: false };

class AmdbFenixOans extends BaseInstrument {
  private readonly bus = new ArincEventBus();

  private readonly backplane = new InstrumentBackplane();

  // Made once the side is known: BaseInstrument reads `Index` from the gauge's address in
  // its own connectedCallback.
  private fenix!: FenixOansPublisher;

  private display!: OansDisplay;

  private taxiRoute!: TaxiRouteFeed;

  /**
   * Display pixels per layout pixel. The display is laid out at 768 x 768, as Fenix's ND
   * is; a panel.cfg that renders the ND texture larger (pixel_size 1536 for sharper text)
   * gives a bigger page, which this fills by scaling the layout up to it.
   */
  private scale = 1;

  get templateID(): string {
    return 'AmdbOansNd';
  }

  get isInteractive(): boolean {
    return true;
  }

  public connectedCallback(): void {
    super.connectedCallback();
    const side = this.instrumentIndex === 2 ? 'R' : 'L';
    const captain = side === 'L';
    reportOnce(`nd-side-${side}`);
    this.fenix = new FenixOansPublisher(this.bus, side);
    this.display = new OansDisplay(this.bus, side);
    this.taxiRoute = new TaxiRouteFeed(this.bus, () => this.display.airport());

    this.backplane.addInstrument('fenix', this.fenix);
    this.fenix.onUiCommand = (name) => {
      reportOnce(`command-${name}`);
      if (name === 'AMDB_OANS_MENU') {
        this.display.openContextMenu(260, 200);
      } else if (name === 'AMDB_OANS_PANEL') {
        this.display.controlPanelVisible.set(!this.display.controlPanelVisible.get());
      } else if (name === 'AMDB_OANS_DUMP_LAYOUT') {
        this.display.dumpLayout();
      }
    };
    // The control panel's airport auto-load runs on the clock's `realTime`, as on the A380X ND.
    this.backplane.addInstrument('clock', new Clock(this.bus));
    this.backplane.addPublisher('fms-oans', new FmsOansSimvarPublisher(this.bus));
    this.backplane.addPublisher('rop-row-oans', new RopRowOansPublisher(this.bus));
    this.backplane.addPublisher('btv', new BtvSimvarPublisher(this.bus));
    this.backplane.addPublisher('resetPanel', new ResetPanelSimvarPublisher(this.bus));

    let btvStatus: Subscribable<BtvStatus>;
    if (captain) {
      const btv = new FenixBtv(this.bus);
      this.backplane.addInstrument('btv-braking', btv);
      btvStatus = btv.status;
      btv.status.sub((s) => this.bus.getPublisher<BtvShared>().pub('amdb_btv_status', s, true, true), true);

      // The BTV exit picked on the map, also for tools outside the simulator.
      const fms = this.bus.getSubscriber<FmsOansData>();
      fms.on('oansExitCoordinates').handle((c) => {
        SimVar.SetSimVarValue('L:AMDB_BTV_EXIT_LAT', 'degrees', c.lat);
        SimVar.SetSimVarValue('L:AMDB_BTV_EXIT_LON', 'degrees', c.long);
      });
      fms.on('oansSelectedExit').handle((exit) => {
        SimVar.SetSimVarValue('L:AMDB_BTV_EXIT_SELECTED', 'number', exit ? 1 : 0);
        reportOnce(`btv-exit-${exit ?? 'cleared'}`);
      });
    } else {
      btvStatus = ConsumerSubject.create(this.bus.getSubscriber<BtvShared>().on('amdb_btv_status'), BTV_IDLE);
    }
    this.backplane.init();

    const content = document.getElementById('OANS_CONTENT') as HTMLElement;
    this.display.render(content);

    // BTV's state, at the top of the ND whether the OANS is showing or not.
    FSComponent.render(
      <svg class="amdb-btv" viewBox="0 0 768 768">
        <text x={384} y={60} class={btvStatus.map(btvClass)}>
          {btvStatus.map(btvText)}
        </text>
      </svg>,
      content,
    );
  }

  public onInteractionEvent(args: string[]): void {
    super.onInteractionEvent(args);
    this.fenix?.command(args[0]);
  }

  public Update(): void {
    super.Update();
    if (!this.display) {
      return;
    }
    this.fitToDisplay();
    this.taxiRoute.update();
    this.backplane.onUpdate();
    this.display.update();
  }

  /** Scale the 768 x 768 layout to the page the display gives this gauge. */
  private fitToDisplay(): void {
    const width = window.innerWidth;
    const scale = width > 0 ? width / 768 : 1;
    if (Math.abs(scale - this.scale) < 0.01) {
      return;
    }
    this.scale = scale;
    const content = document.getElementById('OANS_CONTENT');
    if (content) {
      content.style.transformOrigin = '0 0';
      content.style.transform = scale === 1 ? '' : `scale(${scale})`;
    }
    reportOnce(`display-${width}px-scale-${scale.toFixed(2)}`);
  }
}

registerInstrument('amdb-oans-nd-element', AmdbFenixOans);
