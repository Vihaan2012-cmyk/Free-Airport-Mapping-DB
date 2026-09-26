// SPDX-License-Identifier: GPL-3.0
//
// FlyByWire's A380X OANS (https://github.com/flybywiresim/aircraft, GPL-3.0) running as an
// overlay on the Fenix A320 Captain ND. The OANS, its control panel, the context menu and
// the erase dialogs are FlyByWire's own components (see OansDisplay); what the A380X's
// systems would feed them comes from FenixOansPublisher.

import { Clock, FSComponent, InstrumentBackplane } from '@microsoft/msfs-sdk';
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

class AmdbFenixOans extends BaseInstrument {
  private readonly bus = new ArincEventBus();

  private readonly backplane = new InstrumentBackplane();

  private readonly fenix = new FenixOansPublisher(this.bus);

  private readonly btv = new FenixBtv(this.bus);

  private readonly display = new OansDisplay(this.bus);

  /** The taxi route the bridge keeps, drawn when it is for the airport shown. */
  private readonly taxiRoute = new TaxiRouteFeed(this.bus, () => this.display.airport());

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
    this.backplane.addInstrument('btv-braking', this.btv);
    this.backplane.init();

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

    const content = document.getElementById('OANS_CONTENT') as HTMLElement;
    this.display.render(content);

    // BTV's state, at the top of the ND whether the OANS is showing or not.
    FSComponent.render(
      <svg class="amdb-btv" viewBox="0 0 768 768">
        <text x={384} y={60} class={this.btv.status.map(btvClass)}>
          {this.btv.status.map(btvText)}
        </text>
      </svg>,
      content,
    );
  }

  public onInteractionEvent(args: string[]): void {
    super.onInteractionEvent(args);
    this.fenix.command(args[0]);
  }

  public Update(): void {
    super.Update();
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
