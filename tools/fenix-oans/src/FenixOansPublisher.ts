// SPDX-License-Identifier: GPL-3.0
//
// Feeds FlyByWire's OANS what the A380X's own systems give it -- position and true
// heading as ARINC 429 words, the EFIS ND mode, the OANS zoom and whether to show it --
// from the simulator and the Fenix A320's EFIS controls.
//
// OANS state lives in one L:Var so the knob, the H: events and SimConnect all agree:
//   L:AMDB_OANS_ZOOM  0 = off, 1..5 = 5, 2, 1, 0.5, 0.2 NM (higher is closer in)
// Commands: H:AMDB_OANS_TOGGLE / _RANGE_DEC / _RANGE_INC, or L:AMDB_OANS_CMD = 1 / 2 / 3.

import { Instrument, Publisher } from '@microsoft/msfs-sdk';
import { Arinc429Register, Arinc429SignStatusMatrix, EfisNdMode, FcuSimVars, OansControlEvents } from '@flybywiresim/fbw-sdk';

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

interface PositionWords {
  latitude: number;
  longitude: number;
  trueHeadingRaw: number;
  groundSpeed: number;
  trueAirSpeed: number;
  windDirection: number;
  windSpeed: number;
  ra_radio_altitude_1: number;
  ra_radio_altitude_2: number;
  ra_radio_altitude_3: number;
  lgciu_discrete_word_2_1: number;
  lgciu_discrete_word_2_2: number;
}

/** LGCIU discrete word 2, bit 11: the aircraft is on the ground (what BTV reads). */
const LGCIU_ON_GROUND = 1 << 10;

export class FenixOansPublisher implements Instrument {
  private readonly publisher: Publisher<FcuSimVars & OansControlEvents & PositionWords>;

  private readonly word = Arinc429Register.empty();

  private lastMode = -1;

  private lastZoom = -1;

  constructor(bus: { getPublisher: <T>() => Publisher<T> }) {
    this.publisher = bus.getPublisher<FcuSimVars & OansControlEvents & PositionWords>();
    this.word.setSsm(Arinc429SignStatusMatrix.NormalOperation);
  }

  public init(): void {}

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

    this.publishWord('latitude', SimVar.GetSimVarValue('PLANE LATITUDE', 'degree latitude'));
    this.publishWord('longitude', SimVar.GetSimVarValue('PLANE LONGITUDE', 'degree longitude'));
    this.publishWord('trueHeadingRaw', SimVar.GetSimVarValue('PLANE HEADING DEGREES TRUE', 'degree'));
    this.publishWord('groundSpeed', SimVar.GetSimVarValue('GROUND VELOCITY', 'knots'));
    this.publishWord('trueAirSpeed', SimVar.GetSimVarValue('AIRSPEED TRUE', 'knots'));
    this.publishWord('windDirection', SimVar.GetSimVarValue('AMBIENT WIND DIRECTION', 'degrees'));
    this.publishWord('windSpeed', SimVar.GetSimVarValue('AMBIENT WIND VELOCITY', 'knots'));
    const radioAltitude = SimVar.GetSimVarValue('RADIO HEIGHT', 'feet');
    this.publishWord('ra_radio_altitude_1', radioAltitude);
    this.publishWord('ra_radio_altitude_2', radioAltitude);
    this.publishWord('ra_radio_altitude_3', radioAltitude);
    const gear = SimVar.GetSimVarValue('SIM ON GROUND', 'bool') ? LGCIU_ON_GROUND : 0;
    this.publishWord('lgciu_discrete_word_2_1', gear);
    this.publishWord('lgciu_discrete_word_2_2', gear);

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

  private publishWord(topic: keyof PositionWords, value: number): void {
    this.word.setValue(value);
    this.publisher.pub(topic, this.word.rawWord, false, true);
  }
}
