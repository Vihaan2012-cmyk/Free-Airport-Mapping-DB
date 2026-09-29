// SPDX-License-Identifier: GPL-3.0
//
// Feeds FlyByWire's OANS what the A380X's own systems give it -- the aircraft (see
// AircraftPublisher), the EFIS ND mode, the OANS zoom and whether to show it -- from the
// simulator and the aircraft's EFIS controls, for the captain's ND or the first
// officer's: the Fenix A320's, or those of an aircraft built on FlyByWire's A32NX (the
// Headwind A330), whose FCU keeps the ND mode in FlyByWire's own variables.
//
// Each side's OANS state lives in one L:Var so the knob, the H: events and SimConnect all
// agree:
//   L:AMDB_OANS_ZOOM (captain), L:AMDB_OANS_ZOOM_FO (first officer)
//     0 = off, 1..5 = 5, 2, 1, 0.5, 0.2 NM (higher is closer in)
// Commands: H:AMDB_OANS_TOGGLE / _RANGE_DEC / _RANGE_INC (H:AMDB_OANS_FO_... for the first
// officer), or L:AMDB_OANS_CMD (L:AMDB_OANS_CMD_FO) = 1 / 2 / 3.

import { Publisher } from '@microsoft/msfs-sdk';
import { EfisNdMode, FcuSimVars, OansControlEvents } from '@flybywiresim/fbw-sdk';
import { AircraftPublisher } from './AircraftPublisher';

/** Whose EFIS controls these are: the Fenix A320's, or an A32NX-family FCU's. */
export type Fcu = 'fenix' | 'fbw';

const MAX_ZOOM = 5;
const DEFAULT_ZOOM = 4;
const COMMAND_NAMES = ['', 'AMDB_OANS_TOGGLE', 'AMDB_OANS_RANGE_DEC', 'AMDB_OANS_RANGE_INC', 'AMDB_OANS_MENU', 'AMDB_OANS_PANEL', 'AMDB_OANS_DUMP_LAYOUT'];
/** Commands for the display itself rather than the range, handed to the instrument. */
const UI_COMMANDS = ['AMDB_OANS_MENU', 'AMDB_OANS_PANEL', 'AMDB_OANS_DUMP_LAYOUT'];
const COMMANDS: Record<string, (zoom: number) => number> = {
  AMDB_OANS_TOGGLE: (z) => (z > 0 ? 0 : DEFAULT_ZOOM),
  AMDB_OANS_RANGE_DEC: (z) => (z > 0 ? Math.min(MAX_ZOOM, z + 1) : z),
  AMDB_OANS_RANGE_INC: (z) => (z > 1 ? z - 1 : z),
};
/** The ND modes the A380X shows OANS in; ROSE LS and ROSE VOR keep the normal ND. */
const OANS_MODES = [EfisNdMode.PLAN, EfisNdMode.ARC, EfisNdMode.ROSE_NAV];

export class FenixOansPublisher extends AircraftPublisher {
  private readonly publisher: Publisher<FcuSimVars & OansControlEvents>;

  private lastMode = -1;

  private lastZoom = -1;

  /** This side's variables: the captain's, or the first officer's with `_FO`. */
  private readonly zoomVar: string;

  private readonly cmdVar: string;

  private readonly activeVar: string;

  private readonly modeVar: string;

  constructor(
    bus: { getPublisher: <T>() => Publisher<T> },
    private readonly side: 'L' | 'R' = 'L',
    fcu: Fcu = 'fenix',
  ) {
    super(bus);
    this.publisher = bus.getPublisher<FcuSimVars & OansControlEvents>();
    const fo = side === 'R' ? '_FO' : '';
    this.zoomVar = `L:AMDB_OANS_ZOOM${fo}`;
    this.cmdVar = `L:AMDB_OANS_CMD${fo}`;
    this.activeVar = `L:AMDB_OANS_ACTIVE${fo}`;
    // Both number the mode knob's positions exactly as EfisNdMode does: LS, VOR, NAV, ARC, PLAN.
    this.modeVar = fcu === 'fbw' ? `L:A32NX_EFIS_${side}_ND_MODE` : `L:S_FCU_EFIS${side === 'R' ? 2 : 1}_ND_MODE`;
  }

  /** Called with the display commands (open the menu, toggle the control panel). */
  public onUiCommand: (name: string) => void = () => undefined;

  /**
   * An H: event, or a command number arriving through this side's command variable. The
   * first officer's H: events carry `_FO` after `AMDB_OANS`; each side takes only its own.
   */
  public command(name: string): void {
    const own = this.side === 'R' ? name.startsWith('AMDB_OANS_FO_') : !name.startsWith('AMDB_OANS_FO_');
    if (!own) {
      return;
    }
    const plain = name.replace('AMDB_OANS_FO_', 'AMDB_OANS_');
    if (UI_COMMANDS.includes(plain)) {
      this.onUiCommand(plain);
      return;
    }
    const apply = COMMANDS[plain];
    if (apply) {
      SimVar.SetSimVarValue(this.zoomVar, 'number', apply(Math.round(SimVar.GetSimVarValue(this.zoomVar, 'number'))));
    }
  }

  public onUpdate(): void {
    const cmd = SimVar.GetSimVarValue(this.cmdVar, 'number');
    if (cmd) {
      const name = COMMAND_NAMES[cmd] ?? '';
      this.command(this.side === 'R' ? name.replace('AMDB_OANS_', 'AMDB_OANS_FO_') : name);
      SimVar.SetSimVarValue(this.cmdVar, 'number', 0);
    }

    super.onUpdate();

    const mode = Math.round(SimVar.GetSimVarValue(this.modeVar, 'number'));
    const zoom = Math.round(SimVar.GetSimVarValue(this.zoomVar, 'number'));
    if (mode === this.lastMode && zoom === this.lastZoom) {
      return;
    }
    this.lastMode = mode;
    this.lastZoom = zoom;
    const show = zoom > 0 && OANS_MODES.includes(mode);
    this.publisher.pub('ndMode', mode as EfisNdMode, false, true);
    if (zoom > 0) {
      this.publisher.pub('oansRange', MAX_ZOOM - zoom, false, true);
    }
    this.publisher.pub('nd_show_oans', { side: this.side, show }, false, true);
    SimVar.SetSimVarValue(this.activeVar, 'number', show ? 1 : 0);
  }
}
