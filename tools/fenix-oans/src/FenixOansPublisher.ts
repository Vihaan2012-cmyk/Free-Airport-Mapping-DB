// SPDX-License-Identifier: GPL-3.0
//
// Feeds FlyByWire's OANS what the A380X's own systems give it -- the aircraft (see
// AircraftPublisher), the EFIS ND mode, the OANS zoom and whether to show it -- from the
// simulator and the Fenix A320's EFIS controls.
//
// OANS state lives in one L:Var so the knob, the H: events and SimConnect all agree:
//   L:AMDB_OANS_ZOOM  0 = off, 1..5 = 5, 2, 1, 0.5, 0.2 NM (higher is closer in)
// Commands: H:AMDB_OANS_TOGGLE / _RANGE_DEC / _RANGE_INC, or L:AMDB_OANS_CMD = 1 / 2 / 3.

import { Publisher } from '@microsoft/msfs-sdk';
import { EfisNdMode, FcuSimVars, OansControlEvents } from '@flybywiresim/fbw-sdk';
import { AircraftPublisher } from './AircraftPublisher';

const ZOOM_VAR = 'L:AMDB_OANS_ZOOM';
const CMD_VAR = 'L:AMDB_OANS_CMD';
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

  constructor(bus: { getPublisher: <T>() => Publisher<T> }) {
    super(bus);
    this.publisher = bus.getPublisher<FcuSimVars & OansControlEvents>();
  }

  /** Called with the display commands (open the menu, toggle the control panel). */
  public onUiCommand: (name: string) => void = () => undefined;

  /** An H: event, or a command number arriving through L:AMDB_OANS_CMD. */
  public command(name: string): void {
    if (UI_COMMANDS.includes(name)) {
      this.onUiCommand(name);
      return;
    }
    const apply = COMMANDS[name];
    if (apply) {
      SimVar.SetSimVarValue(ZOOM_VAR, 'number', apply(Math.round(SimVar.GetSimVarValue(ZOOM_VAR, 'number'))));
    }
  }

  public onUpdate(): void {
    const cmd = SimVar.GetSimVarValue(CMD_VAR, 'number');
    if (cmd) {
      this.command(COMMAND_NAMES[cmd] ?? '');
      SimVar.SetSimVarValue(CMD_VAR, 'number', 0);
    }

    super.onUpdate();

    // Fenix's mode knob numbers its positions exactly as EfisNdMode does: LS, VOR, NAV, ARC, PLAN.
    const mode = Math.round(SimVar.GetSimVarValue('L:S_FCU_EFIS1_ND_MODE', 'number'));
    const zoom = Math.round(SimVar.GetSimVarValue(ZOOM_VAR, 'number'));
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
    this.publisher.pub('nd_show_oans', { side: 'L', show }, false, true);
    SimVar.SetSimVarValue('L:AMDB_OANS_ACTIVE', 'number', show ? 1 : 0);
  }
}
