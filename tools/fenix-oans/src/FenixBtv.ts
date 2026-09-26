// SPDX-License-Identifier: GPL-3.0
//
// Brake to vacate for the Fenix A320: the exit picked on the OANS, reached at taxi speed.
//
// Armed with ARM BTV on the OANS control panel's MAP DATA page (or L:AMDB_BTV_ARM = 1)
// once an exit is picked on the OANS; Fenix's autobrake need not be armed, and LO and MED
// cannot be on the ground. At the rollout (on the ground above 30 kt with the ground
// spoilers out, or slowing at idle) it presses any lit autobrake button off, before
// Fenix's autobrake starts braking, and works the brake pedals itself:
//
// - no braking while the aircraft would reach the exit on its own (it only ever brakes as
//   much as the exit needs; BTV cannot add thrust);
// - once the deceleration needed reaches the chosen rate (2 m/s2, or 3 when 2 would not
//   do), it brakes for 10 kt at the exit (v^2 = v_r^2 + 2ad, plus 5%);
// - it lets go at 10 kt, or on passing the exit, the parking brake, thrust, the autobrake
//   being armed again, the exit being cleared, or leaving the ground.
//
// The law is FlyByWire's A380X BtvDecelScheduler (fbw-a380x/src/wasm/systems/a380_systems/
// src/hydraulic/autobrakes.rs), changed where the Fenix differs, as measured: the release
// is at the exit rather than 65.5 m before it (the Fenix slows by itself at idle and
// stopped short from there), and nothing is braked while coasting (FBW holds -0.2 m/s2).
//
//   L:AMDB_BTV_ARM    1 while BTV is to be used; cleared after each rollout
//   L:AMDB_BTV_STATE  0 off, 1 armed, 2 rolling out, 3 braking, 4 holding the final rate,
//                     5 letting go

import { EventBus, Instrument, Publisher, Subject } from '@microsoft/msfs-sdk';
import { FmsOansData } from '@flybywiresim/fbw-sdk';

const KT = 0.514444;
const RELEASE_SPEED = 5.15; // 10 kt
const RELEASE_TARGET = RELEASE_SPEED * 0.9;
const RELEASE_BEFORE_EXIT_M = 10;
const HOLD_RATE_WITHIN_M = 15;
const RATE_WET = -2.0;
const RATE_DRY = -3.0;
const START_AT = 0.98;
const MARGIN = 1.05;
const ARM_SPEED = 30 * KT;
const PEDAL_RATE_PER_S = 1.0;
const TAKEOVER_CHECK_S = 0.5;
const LIMIT_S = 90;

export enum BtvState {
  Off = 0,
  Armed = 1,
  /** Rolling out without braking: the exit is still further than the aircraft would roll. */
  Rolling = 2,
  Braking = 3,
  /** The last metres: the rate reached is held, as FBW's end of braking. */
  Holding = 4,
  LettingGo = 5,
}

const ARM_VAR = 'L:AMDB_BTV_ARM';

/** Fenix's autobrake modes, by the ON light and the pushbutton of each. */
const AUTOBRAKE = [
  { light: 'L:I_MIP_AUTOBRAKE_LO_L', button: 'L:S_MIP_AUTOBRAKE_LO', name: 'LO' },
  { light: 'L:I_MIP_AUTOBRAKE_MED_L', button: 'L:S_MIP_AUTOBRAKE_MED', name: 'MED' },
  { light: 'L:I_MIP_AUTOBRAKE_MAX_L', button: 'L:S_MIP_AUTOBRAKE_MAX', name: 'MAX' },
];

/** Topics this publishes for the ARM BTV button on the OANS control panel. */
export interface BtvArmEvents {
  /** BTV is armed (by the button or L:AMDB_BTV_ARM). */
  amdb_btv_armed: boolean;
  /** An exit is picked, so BTV can be armed. */
  amdb_btv_can_arm: boolean;
}

interface Reading {
  time: number;
  gs: number;
  lat: number;
  lon: number;
  heading: number;
  accel: number;
  onGround: boolean;
  parkingBrake: number;
  spoilers: number;
  autobrake: (typeof AUTOBRAKE)[number] | undefined;
  armRequested: boolean;
}

export interface BtvStatus {
  state: BtvState;
  exit: string | null;
  metres: number | null;
  missed: boolean;
}

export class FenixBtv implements Instrument {
  public readonly status = Subject.create<BtvStatus>({ state: BtvState.Off, exit: null, metres: null, missed: false });

  private exit: { name: string; lat: number; lon: number } | null = null;

  private exitName: string | null = null;

  private exitAt: { lat: number; long: number } | null = null;

  private state = BtvState.Off;

  /** One try per rollout: set once it has been tried (or refused), cleared below 25 kt. */
  private rolloutDone = false;

  private takeoverAt: number | null = null;

  private startedAt = 0;

  private lastTime = 0;

  private slowingSince: number | null = null;

  private desired = RATE_WET;

  private endRate = 0;

  private missed = false;

  private decel = 0;

  private integral = 0;

  private pedal = 0;

  private releaseFrames = 0;

  private nextLog = 0;

  private readonly pub: Publisher<BtvArmEvents>;

  private published: { armed?: boolean; canArm?: boolean } = {};

  constructor(bus: EventBus) {
    this.pub = bus.getPublisher<BtvArmEvents>();
    const sub = bus.getSubscriber<FmsOansData>();
    sub.on('oansExitCoordinates').handle((c: { lat: number; long: number }) => {
      this.exitAt = c;
      this.updateExit();
    });
    sub.on('oansSelectedExit').handle((name: string | null) => {
      this.exitName = name;
      this.updateExit();
    });
  }

  public init(): void {}

  private updateExit(): void {
    this.exit = this.exitName && this.exitAt ? { name: this.exitName, lat: this.exitAt.lat, lon: this.exitAt.long } : null;
  }

  private read(): Reading {
    return {
      time: SimVar.GetSimVarValue('E:SIMULATION TIME', 'seconds'),
      gs: SimVar.GetSimVarValue('GROUND VELOCITY', 'meters per second'),
      lat: SimVar.GetSimVarValue('PLANE LATITUDE', 'degree latitude'),
      lon: SimVar.GetSimVarValue('PLANE LONGITUDE', 'degree longitude'),
      heading: SimVar.GetSimVarValue('PLANE HEADING DEGREES TRUE', 'degree'),
      accel: SimVar.GetSimVarValue('ACCELERATION BODY Z', 'meters per second squared'),
      onGround: !!SimVar.GetSimVarValue('SIM ON GROUND', 'bool'),
      parkingBrake: SimVar.GetSimVarValue('BRAKE PARKING POSITION', 'percent over 100'),
      spoilers: SimVar.GetSimVarValue('SPOILERS LEFT POSITION', 'percent over 100'),
      autobrake: AUTOBRAKE.find((a) => SimVar.GetSimVarValue(a.light, 'number') > 0.5),
      armRequested: SimVar.GetSimVarValue(ARM_VAR, 'number') > 0.5,
    };
  }

  /** What the ARM BTV button shows, sent only when it changes. */
  private publishArm(armed: boolean, canArm: boolean): void {
    if (this.published.armed !== armed) {
      this.published.armed = armed;
      this.pub.pub('amdb_btv_armed', armed, false, true);
    }
    if (this.published.canArm !== canArm) {
      this.published.canArm = canArm;
      this.pub.pub('amdb_btv_can_arm', canArm, false, true);
    }
  }

  /** Along-track metres from the aircraft to the exit; negative once passed. */
  private ahead(r: Reading): number {
    if (!this.exit) {
      return -Infinity;
    }
    const rad = Math.PI / 180;
    const p1 = r.lat * rad;
    const p2 = this.exit.lat * rad;
    const dl = (this.exit.lon - r.lon) * rad;
    const a = Math.sin((p2 - p1) / 2) ** 2 + Math.cos(p1) * Math.cos(p2) * Math.sin(dl / 2) ** 2;
    const d = 2 * 6371000 * Math.asin(Math.sqrt(a));
    const bearing = Math.atan2(Math.sin(dl) * Math.cos(p2), Math.cos(p1) * Math.sin(p2) - Math.sin(p1) * Math.cos(p2) * Math.cos(dl));
    return d * Math.cos(bearing - r.heading * rad);
  }

  public onUpdate(): void {
    const r = this.read();
    const dt = r.time - this.lastTime;
    this.lastTime = r.time;
    if (dt <= 0 || dt > 1) {
      // Paused, or a jump in time (a slew, a reload): hold everything as it is.
      return;
    }
    if (r.gs < 25 * KT) {
      this.rolloutDone = false;
    }
    // Armed without an exit means nothing: it waits for one to be picked.
    this.publishArm(r.armRequested, !!this.exit || r.armRequested);

    if (this.state === BtvState.Off || this.state === BtvState.Armed) {
      this.watch(r);
    } else {
      this.rollout(r, dt);
    }

    const ahead = this.ahead(r);
    const active = this.state >= BtvState.Rolling;
    this.publish({
      state: this.state,
      exit: this.exit?.name ?? null,
      metres: active && Number.isFinite(ahead) ? Math.max(0, Math.round(ahead)) : null,
      missed: active && this.missed,
    });
  }

  /** Armed or not, and the start of the rollout. */
  private watch(r: Reading): void {
    if (this.takeoverAt !== null) {
      // The autobrake button was pressed: it has the brakes only if its light went out.
      if (r.time - this.takeoverAt < TAKEOVER_CHECK_S) {
        return;
      }
      this.takeoverAt = null;
      if (r.autobrake) {
        log(`takeover-failed-${r.autobrake.name}-still-armed:-Fenix-autobrake-keeps-the-brakes`);
        return;
      }
      this.begin(r);
      return;
    }

    this.setState(this.exit && r.armRequested ? BtvState.Armed : BtvState.Off);
    const rolling = r.onGround && r.gs > ARM_SPEED;
    this.slowingSince = rolling && r.accel < -0.15 ? (this.slowingSince ?? r.time) : null;
    const started = rolling && (r.spoilers > 0.5 || (this.slowingSince !== null && r.time - this.slowingSince >= 1));
    if (!started || this.rolloutDone || this.state !== BtvState.Armed) {
      return;
    }
    this.rolloutDone = true;
    if (this.ahead(r) <= RELEASE_BEFORE_EXIT_M) {
      log(`exit-already-passed:-not-taking-over`);
      return;
    }
    if (!r.autobrake) {
      this.begin(r);
      return;
    }
    // Take the brakes from Fenix's autobrake before it starts (2-4 s after the spoilers):
    // press its lit button, as a finger would, and check it went off. Fenix's momentary
    // buttons count: a press adds one (odd, held) and the release another (even).
    const button = r.autobrake.button;
    const count = Math.round(SimVar.GetSimVarValue(button, 'number'));
    const released = count % 2 === 0 ? count : count + 1;
    SimVar.SetSimVarValue(button, 'number', released + 1);
    setTimeout(() => SimVar.SetSimVarValue(button, 'number', released + 2), 150);
    this.takeoverAt = r.time;
    log(`takeover:-${r.autobrake.name}-pressed-off-at-${Math.round(r.gs / KT)}kt-exit-${Math.round(this.ahead(r))}m`);
  }

  private begin(r: Reading): void {
    const remaining = Math.max(1, this.ahead(r) - RELEASE_BEFORE_EXIT_M);
    const need = (-(r.gs ** 2 - RELEASE_TARGET ** 2) / (2 * remaining)) * MARGIN;
    // As FBW: the wet rate unless only the dry one gets there.
    this.desired = need < RATE_WET ? RATE_DRY : RATE_WET;
    this.startedAt = r.time;
    this.decel = r.accel;
    this.integral = 0;
    this.pedal = 0;
    this.nextLog = 0;
    this.missed = false;
    this.setState(BtvState.Rolling);
    log(`active:-${Math.round(r.gs / KT)}kt-exit-${this.exit?.name}-${Math.round(this.ahead(r))}m-rate-${this.desired}`);
  }

  private rollout(r: Reading, dt: number): void {
    if (this.state === BtvState.LettingGo) {
      // A few frames of released pedals, so the last command is surely "off".
      this.brake(0);
      if (--this.releaseFrames <= 0) {
        // One rollout per arming: armed again for the next landing.
        SimVar.SetSimVarValue(ARM_VAR, 'number', 0);
        this.setState(BtvState.Off);
      }
      return;
    }

    const ahead = this.ahead(r);
    const stop = !r.onGround
      ? 'left-the-ground'
      : r.parkingBrake > 0.5
        ? 'parking-brake'
        : r.time - this.startedAt > 2 && r.accel > 0.5
          ? 'thrust'
          : r.autobrake
            ? `autobrake-${r.autobrake.name}-armed-again`
            : !this.exit
              ? 'exit-cleared'
              : r.gs <= RELEASE_SPEED
                ? '10kt'
                : ahead < -30
                  ? 'exit-passed'
                  : r.time - this.startedAt > LIMIT_S
                    ? 'time-limit'
                    : null;
    if (stop) {
      log(`released:-${stop}-at-${(r.gs / KT).toFixed(1)}kt-exit-${Math.round(ahead)}m`);
      this.releaseFrames = 5;
      this.setState(BtvState.LettingGo);
      this.brake(0);
      return;
    }

    this.decel += (r.accel - this.decel) * Math.min(1, dt / 0.25);
    const remaining = Math.max(0, ahead - RELEASE_BEFORE_EXIT_M);
    const need = -Math.max(0, r.gs ** 2 - RELEASE_TARGET ** 2) / (2 * Math.max(0.5, remaining));
    const request = Math.min(5, Math.max(this.desired, need * MARGIN));
    if (this.state === BtvState.Rolling && request < this.desired * START_AT) {
      this.setState(BtvState.Braking);
    } else if (this.state === BtvState.Braking && remaining < HOLD_RATE_WITHIN_M) {
      this.endRate = request;
      this.setState(BtvState.Holding);
    }
    const target = this.state === BtvState.Rolling ? null : this.state === BtvState.Holding ? this.endRate : request;
    // Not even the dry rate reaches the exit at 10 kt.
    this.missed = need * MARGIN < RATE_DRY && r.gs - RELEASE_TARGET > RELEASE_SPEED;

    let want = 0;
    if (target === null) {
      this.integral = 0;
    } else {
      const err = this.decel - target;
      this.integral = Math.min(1, Math.max(0, this.integral + 0.6 * err * dt));
      want = Math.min(1, Math.max(0, this.integral + 0.15 * err));
    }
    this.pedal += Math.min(PEDAL_RATE_PER_S * dt, Math.max(-PEDAL_RATE_PER_S * dt, want - this.pedal));
    this.brake(this.pedal);

    if (r.time >= this.nextLog) {
      this.nextLog = r.time + 1;
      log(`t${(r.time - this.startedAt).toFixed(1)}-s${this.state}-${(r.gs / KT).toFixed(1)}kt-${Math.round(ahead)}m-tgt${target === null ? 'none' : target.toFixed(2)}-dec${this.decel.toFixed(2)}-ped${Math.round(this.pedal * 100)}`);
    }
  }

  /** Both brake pedals, 0 (released) to 1 (fully pressed), as hardware toe brakes send them. */
  private brake(u: number): void {
    const axis = Math.round(-16383 + 32766 * Math.min(1, Math.max(0, u)));
    SimVar.SetSimVarValue('K:AXIS_LEFT_BRAKE_SET', 'number', axis);
    SimVar.SetSimVarValue('K:AXIS_RIGHT_BRAKE_SET', 'number', axis);
  }

  private setState(state: BtvState): void {
    if (state !== this.state) {
      this.state = state;
      SimVar.SetSimVarValue('L:AMDB_BTV_STATE', 'number', state);
    }
  }

  private publish(s: BtvStatus): void {
    const now = this.status.get();
    if (now.state !== s.state || now.exit !== s.exit || now.metres !== s.metres || now.missed !== s.missed) {
      this.status.set(s);
    }
  }
}

/** A line in the bridge's log, for following a rollout from outside the simulator. */
function log(what: string): void {
  fetch(`http://127.0.0.1:8770/amdb-oans-event?btv-${what}`).catch(() => undefined);
}
