// SPDX-License-Identifier: GPL-3.0
//
// FlyByWire's A380X OANS (https://github.com/flybywiresim/aircraft, GPL-3.0) running as an
// overlay on the Fenix A320 Captain ND. The OANS, its control panel, the context menu and
// the erase dialogs are FlyByWire's own components, mounted the way their A380X ND mounts
// them; what the A380X's systems would feed them comes from FenixOansPublisher.

import { Clock, FSComponent, InstrumentBackplane, MappedSubject, Subject } from '@microsoft/msfs-sdk';
import { Arinc429Register, ArincEventBus, BtvSimvarPublisher, EfisNdMode, FmsOansData, FmsOansSimvarPublisher, OansControlEvents } from '@flybywiresim/fbw-sdk';
import { a380EfisZoomRangeSettings, A380EfisZoomRangeValue, Oanc } from '@flybywiresim/oanc';
import { RopRowOansPublisher } from '@flybywiresim/msfs-avionics-common';
import { ContextMenu, ContextMenuElement } from '@a380x/MsfsAvionicsCommon/UiWidgets/ContextMenu';
import { ResetPanelSimvarPublisher } from '@a380x/MsfsAvionicsCommon/providers/ResetPanelPublisher';
import { EraseSymbolsDialog, OansControlPanel } from '@a380x/ND/OansControlPanel';
import { WindIndicator } from '@fbw-nd/shared/WindIndicator';
import { FenixOansPublisher } from './FenixOansPublisher';
import { BtvState, BtvStatus, FenixBtv } from './FenixBtv';

import '@a380x/ND/style.scss';
import '@a380x/ND/oans-style.scss';
import './fenix.scss';

/** Tell the bridge log, once per kind, which mouse events MSFS delivers to this panel. */
const reported = new Set<string>();
function reportOnce(what: string): void {
  if (!reported.has(what)) {
    reported.add(what);
    fetch(`http://127.0.0.1:8770/amdb-oans-event?${what}`).catch(() => undefined);
  }
}

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

  private readonly oansRef = FSComponent.createRef<Oanc<A380EfisZoomRangeValue>>();

  private readonly controlPanelRef = FSComponent.createRef<OansControlPanel>();

  private readonly contextMenuRef = FSComponent.createRef<ContextMenu>();

  private readonly oansShown = Subject.create(false);

  private readonly contextMenuVisible = Subject.create(false);

  private readonly contextMenuX = Subject.create(0);

  private readonly contextMenuY = Subject.create(0);

  private readonly contextMenuOpened = Subject.create(false);

  private contextMenuAt = { x: 0, y: 0 };

  private readonly controlPanelVisible = Subject.create(false);

  private readonly eraseAllCrossesDialogVisible = Subject.create(false);

  private readonly eraseAllFlagsDialogVisible = Subject.create(false);

  private eraseCrossIndex: number | null = null;

  private eraseFlagIndex: number | null = null;

  private readonly contextMenuItems = Subject.create(this.contextMenu(false, false));

  // The ND frame the A380X draws over its OANS from its normal ND: GS / TAS and wind, and
  // here also the range on the half-range ring, which the OANS compass leaves unlabelled.
  private readonly groundSpeed = Subject.create('');

  private readonly trueAirSpeed = Subject.create('');

  private readonly halfRange = Subject.create('');

  private readonly arcView = Subject.create(false);

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
        this.openContextMenu(260, 200);
      } else if (name === 'AMDB_OANS_PANEL') {
        this.controlPanelVisible.set(!this.controlPanelVisible.get());
      } else if (name === 'AMDB_OANS_DUMP_LAYOUT') {
        this.dumpLayout();
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

    FSComponent.render(
      <div class="amdb-oans-root" style={{ display: this.oansShown.map((v) => (v ? 'block' : 'none')) }}>
        <div class="oanc-container">
          <Oanc
            bus={this.bus}
            side="L"
            ref={this.oansRef}
            contextMenuVisible={this.contextMenuVisible}
            contextMenuX={this.contextMenuX}
            contextMenuY={this.contextMenuY}
            zoomValues={a380EfisZoomRangeSettings}
          />
        </div>
        <svg class="amdb-oans-frame" viewBox="0 0 768 768">
          <g transform="translate(2, 25)">
            <text x={0} y={0} class="White FontSmallest">
              GS
            </text>
            <text x={89} y={0} class="Green FontIntermediate EndAlign">
              {this.groundSpeed}
            </text>
            <text x={95} y={0} class="White FontSmallest">
              TAS
            </text>
            <text x={201} y={0} class="Green FontIntermediate EndAlign">
              {this.trueAirSpeed}
            </text>
          </g>
          <WindIndicator bus={this.bus} />
          <g visibility={this.arcView.map((v) => (v ? 'visible' : 'hidden'))}>
            <text x={175} y={528} class="Cyan FontSmallest">
              {this.halfRange}
            </text>
            <text x={592} y={528} class="Cyan FontSmallest EndAlign">
              {this.halfRange}
            </text>
          </g>
        </svg>
        <ContextMenu ref={this.contextMenuRef} opened={this.contextMenuOpened} idPrefix="contextMenu" values={this.contextMenuItems} />
        <EraseSymbolsDialog
          visible={MappedSubject.create(([c, f]) => c && !f, this.eraseAllCrossesDialogVisible, this.eraseAllFlagsDialogVisible)}
          confirmAction={() => this.bus.getPublisher<OansControlEvents>().pub('oans_erase_all_crosses', true, true, false)}
          hideDialog={() => this.eraseAllCrossesDialogVisible.set(false)}
          isCross={true}
        />
        <EraseSymbolsDialog
          visible={MappedSubject.create(([c, f]) => !c && f, this.eraseAllCrossesDialogVisible, this.eraseAllFlagsDialogVisible)}
          confirmAction={() => this.bus.getPublisher<OansControlEvents>().pub('oans_erase_all_flags', true, true, false)}
          hideDialog={() => this.eraseAllFlagsDialogVisible.set(false)}
          isCross={false}
        />
        <OansControlPanel
          ref={this.controlPanelRef}
          bus={this.bus}
          side="L"
          isVisible={this.controlPanelVisible}
          togglePanel={() => this.controlPanelVisible.set(!this.controlPanelVisible.get())}
        />
      </div>,
      document.getElementById('OANS_CONTENT'),
    );

    // BTV's state, at the top of the ND whether the OANS is showing or not.
    FSComponent.render(
      <svg class="amdb-btv" viewBox="0 0 768 768">
        <text x={384} y={60} class={this.btv.status.map(btvClass)}>
          {this.btv.status.map(btvText)}
        </text>
      </svg>,
      document.getElementById('OANS_CONTENT'),
    );

    // The A380X opens the context menu with the KCCU. On Fenix MSFS delivers mousedown and
    // click to this panel but never dblclick or contextmenu, so: a left click on the map that
    // is not on a label and does not end a drag toggles the menu, and a right-button press
    // opens it. A click on a label stays the label's own (runway and exit selection for BTV).
    // A press is judged from mousedown to mouseup (or, failing a mouseup, the click that
    // follows), using the press's own position: the synthesised click is not trusted for it.
    // Movement is measured in screen coordinates, as FlyByWire's own panning does. Only the
    // labels that act on a click (runway ends and exits for BTV, crosses, flags) keep it.
    const labels = this.oansRef.instance.labelContainerRef.instance;
    const onActiveLabel = (e: MouseEvent) => {
      const target = e.target as HTMLElement | null;
      const label = target && target !== labels && typeof target.closest === 'function' ? target.closest('.oanc-label') : null;
      return label !== null && /runway-end|runway-arrow|exit-line|cross-symbol|flag-symbol/.test(label.className);
    };
    let press: { x: number; y: number; sx: number; sy: number; onLabel: boolean; moved: boolean } | null = null;
    const release = () => {
      const p = press;
      press = null;
      if (!p) {
        return;
      }
      if (p.onLabel || p.moved) {
        reportOnce(p.onLabel ? 'release-on-label' : 'release-after-drag');
        return;
      }
      if (this.contextMenuOpened.get()) {
        this.contextMenuRef.instance.hideMenu();
      } else {
        this.openContextMenu(p.x, p.y);
      }
    };
    labels.addEventListener('mousedown', (e) => {
      reportOnce(`mousedown-button-${e.button}`);
      if (e.button === 2) {
        press = null;
        if (!onActiveLabel(e)) {
          this.openContextMenu(e.clientX / this.scale, e.clientY / this.scale);
        }
        return;
      }
      press = { x: e.clientX / this.scale, y: e.clientY / this.scale, sx: e.screenX, sy: e.screenY, onLabel: onActiveLabel(e), moved: false };
    });
    labels.addEventListener('mousemove', (e) => {
      if (press && Math.hypot(e.screenX - press.sx, e.screenY - press.sy) > 10) {
        press.moved = true;
      }
    });
    labels.addEventListener('mouseup', (e) => {
      reportOnce(`mouseup-button-${e.button}`);
      if (e.button === 0) {
        release();
      }
    });
    labels.addEventListener('click', (e) => {
      reportOnce(`click-at-${Math.round(e.clientX)},${Math.round(e.clientY)}`);
      release();
    });

    const sub = this.bus.getSubscriber<OansControlEvents & { groundSpeed: number; trueAirSpeed: number; ndMode: EfisNdMode; oansRange: number }>();
    sub.on('nd_show_oans').handle(({ show }) => {
      this.oansShown.set(show);
      if (show) {
        this.placeForMode();
        setTimeout(() => this.reportLayout(), 3000);
      }
    });
    const speed = (raw: number) => {
      const word = Arinc429Register.empty().set(raw);
      return word.isNormalOperation() ? Math.round(word.value).toString() : '';
    };
    sub.on('groundSpeed').handle((raw) => this.groundSpeed.set(speed(raw)));
    sub.on('trueAirSpeed').handle((raw) => this.trueAirSpeed.set(speed(raw)));
    sub.on('ndMode').handle((mode) => {
      this.arcView.set(mode === EfisNdMode.ARC);
      this.placeForMode();
    });
    sub.on('oansRange').handle((index) => this.halfRange.set(String((a380EfisZoomRangeSettings[index] ?? 0) / 2)));
    sub.on('oans_answer_symbols_at_cursor').handle((symbols) => {
      if (symbols.side === 'L') {
        this.eraseCrossIndex = symbols.cross;
        this.eraseFlagIndex = symbols.flag;
        this.contextMenuItems.set(this.contextMenu(symbols.cross !== null, symbols.flag !== null));
      }
    });
  }

  /**
   * Put the aircraft where the ND mode wants it: low on the display in ARC (y 620, where
   * the arc is centred), in the middle in ROSE and PLAN. FlyByWire's mode handler sets
   * this same offset, but on Fenix it was found at 0 in ARC, leaving the lower third of
   * the display behind the aircraft; the value found is reported before it is set.
   */
  private placeForMode(): void {
    const oanc = this.oansRef.getOrDefault();
    if (!oanc) {
      return;
    }
    const wanted = this.arcView.get() ? 620 - 768 / 2 : 0;
    const found = oanc.modeAnimationOffsetY.get();
    reportOnce(`mode-offset-found-${found}-wanted-${wanted}`);
    oanc.modeAnimationOffsetX.set(0);
    oanc.modeAnimationOffsetY.set(wanted);
  }

  /** Where the map's moving containers and the aircraft actually ended up, for the bridge log. */
  private reportLayout(): void {
    const root = document.querySelector('.amdb-oans-root .oanc-container');
    if (!root) {
      return;
    }
    const moving = Array.from(root.querySelectorAll('[style*="transform"]')).slice(0, 6) as HTMLElement[];
    const transforms = moving.map((el) => window.getComputedStyle(el).transform.replace(/\s/g, '')).join('|');
    const plane = root.querySelector('.oanc-airplane')?.closest('svg')?.getBoundingClientRect();
    const where = plane ? `${Math.round(plane.left + plane.width / 2)},${Math.round(plane.top + plane.height / 2)}` : 'none';
    reportOnce(`layout-aircraft-${where}-transforms-${transforms}`);
  }

  /** Every positioned element of the map, with where it really is, sent to the bridge log. */
  private dumpLayout(): void {
    const root = document.querySelector('.amdb-oans-root .oanc-container');
    const oanc = this.oansRef.getOrDefault();
    if (!root || !oanc) {
      return;
    }
    const send = (line: string) => fetch(`http://127.0.0.1:8770/amdb-oans-layout?${encodeURIComponent(line)}`).catch(() => undefined);
    const box = (el: Element) => {
      const r = el.getBoundingClientRect();
      return `${Math.round(r.left)},${Math.round(r.top)} ${Math.round(r.width)}x${Math.round(r.height)}`;
    };
    send(`mode=${this.arcView.get() ? 'ARC' : 'not-arc'} offset=${oanc.modeAnimationOffsetX.get()},${oanc.modeAnimationOffsetY.get()} pan=${oanc.panOffsetX.get()},${oanc.panOffsetY.get()}`);
    const plane = root.querySelector('.oanc-airplane')?.closest('svg');
    if (plane) {
      send(`aircraft box=${box(plane)} visibility=${window.getComputedStyle(plane).visibility}`);
    }
    Array.from(root.querySelectorAll('div, canvas, svg'))
      .filter((el) => (el as HTMLElement).style && ((el as HTMLElement).style.transform || (el as HTMLElement).style.clipPath || el.getAttribute('clip-path') || el.tagName === 'CANVAS'))
      .slice(0, 24)
      .forEach((el, i) => {
        const s = window.getComputedStyle(el);
        const clip = (el as HTMLElement).style.clipPath || el.getAttribute('clip-path') || '';
        send(`#${i} ${el.tagName.toLowerCase()}.${(el as HTMLElement).className || '-'} box=${box(el)} vis=${s.visibility} transform=${s.transform.replace(/\s/g, '')} clip=${clip}`);
      });
  }

  private openContextMenu(x: number, y: number): void {
    reportOnce(`menu-opened-at-${Math.round(x)},${Math.round(y)}`);
    this.bus.getPublisher<OansControlEvents>().pub('oans_query_symbols_at_cursor', { side: 'L', cursorPosition: [x, y] });
    this.contextMenuAt = { x, y };
    this.contextMenuRef.instance.display(x, y);
    // Keep the whole menu on the display, whatever coordinates the press came with.
    const menu = document.querySelector('.amdb-oans-root .mfd-context-menu') as HTMLElement | null;
    if (menu) {
      const size = 768;
      const w = menu.offsetWidth;
      const h = menu.offsetHeight;
      const left = Number.isFinite(x) ? Math.max(0, Math.min(size - w, x)) : (size - w) / 2;
      const top = Number.isFinite(y) ? Math.max(0, Math.min(size - h, y)) : (size - h) / 2;
      menu.style.left = `${left}px`;
      menu.style.top = `${top}px`;
      const r = menu.getBoundingClientRect();
      const shown = window.getComputedStyle(menu);
      reportOnce(
        `menu-box-${Math.round(r.left)},${Math.round(r.top)}-${Math.round(r.width)}x${Math.round(r.height)}` +
          `-items-${menu.querySelectorAll('.mfd-context-menu-element').length}-display-${shown.display}-visibility-${shown.visibility}-z-${shown.zIndex}`,
      );
    } else {
      reportOnce('menu-element-missing');
    }
  }

  private contextMenu(hasCross: boolean, hasFlag: boolean): ContextMenuElement[] {
    const pub = () => this.bus.getPublisher<OansControlEvents>();
    const at = (): [number, number] => [this.contextMenuAt.x, this.contextMenuAt.y];
    return [
      {
        name: hasCross ? 'DELETE CROSS' : 'ADD CROSS',
        disabled: false,
        onPressed: () => (hasCross && this.eraseCrossIndex !== null ? pub().pub('oans_erase_cross_id', this.eraseCrossIndex) : pub().pub('oans_add_cross_at_cursor', at())),
      },
      {
        name: hasFlag ? 'DELETE FLAG' : 'ADD FLAG',
        disabled: false,
        onPressed: () => (hasFlag && this.eraseFlagIndex !== null ? pub().pub('oans_erase_flag_id', this.eraseFlagIndex) : pub().pub('oans_add_flag_at_cursor', at())),
      },
      { name: 'MAP DATA', disabled: false, onPressed: () => this.controlPanelVisible.set(!this.controlPanelVisible.get()) },
      { name: 'ERASE ALL CROSSES', disabled: false, onPressed: () => this.eraseAllCrossesDialogVisible.set(true) },
      { name: 'ERASE ALL FLAGS', disabled: false, onPressed: () => this.eraseAllFlagsDialogVisible.set(true) },
      { name: 'CENTER ON ACFT', disabled: false, onPressed: () => pub().pub('oans_center_on_acft', true, true, false) },
    ];
  }

  public onInteractionEvent(args: string[]): void {
    super.onInteractionEvent(args);
    this.fenix.command(args[0]);
  }

  public Update(): void {
    super.Update();
    this.fitToDisplay();
    this.backplane.onUpdate();
    this.oansRef.getOrDefault()?.Update();
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
