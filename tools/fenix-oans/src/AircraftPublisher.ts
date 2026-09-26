// SPDX-License-Identifier: GPL-3.0
//
// What the A380X's own systems give its OANS about the aircraft -- position and true
// heading, speeds, wind, radio height and whether it is on the ground, as ARINC 429
// words -- read from the simulator, which knows them for any aircraft.

import { Instrument, Publisher } from '@microsoft/msfs-sdk';
import { Arinc429Register, Arinc429SignStatusMatrix } from '@flybywiresim/fbw-sdk';

export interface PositionWords {
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

export class AircraftPublisher implements Instrument {
  private readonly words: Publisher<PositionWords>;

  private readonly word = Arinc429Register.empty();

  constructor(bus: { getPublisher: <T>() => Publisher<T> }) {
    this.words = bus.getPublisher<PositionWords>();
    this.word.setSsm(Arinc429SignStatusMatrix.NormalOperation);
  }

  public init(): void {}

  public onUpdate(): void {
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
  }

  private publishWord(topic: keyof PositionWords, value: number): void {
    this.word.setValue(value);
    this.words.pub(topic, this.word.rawWord, false, true);
  }
}
