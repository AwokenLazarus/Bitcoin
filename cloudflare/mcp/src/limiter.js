// Exact rate limiting. One Durable Object per key (a client IP, or the single key "origin"), so
// every request for that key meets the same counter wherever it entered Cloudflare.
//
// Workers' `ratelimit` binding was tried first and measured: its counters live per Worker
// instance and sync loosely, and 45 calls in a row against a limit of 8 were all allowed. That is
// fine for shaving abuse, not for a promise to the hub, so the counting is done here instead.
//
// State is memory only. If the object is evicted the windows restart, which errs on the side of
// allowing a request; nothing here needs to survive.
import { DurableObject } from "cloudflare:workers";

export class Limiter extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.windows = new Map(); // rule name -> { slot, used }
  }

  /** rules: [{ name, limit, period_s }]. All pass and are counted, or the first one over is reported and nothing is counted. */
  take(rules) {
    const now = Date.now() / 1000;
    for (const r of rules) {
      const slot = Math.floor(now / r.period_s), w = this.windows.get(r.name);
      if (w && w.slot === slot && w.used >= r.limit) return { ok: false, rule: r.name, retry_s: Math.ceil((slot + 1) * r.period_s - now) };
    }
    for (const r of rules) {
      const slot = Math.floor(now / r.period_s), w = this.windows.get(r.name);
      if (w && w.slot === slot) w.used++;
      else this.windows.set(r.name, { slot, used: 1 });
    }
    return { ok: true };
  }
}

export const RULES = {
  client: { name: "client", limit: 30, period_s: 60 }, // every tool call, per client IP
  heavy: { name: "heavy", limit: 8, period_s: 60 }, // per-address lookups, per client IP
  origin: { name: "origin", limit: 240, period_s: 60 }, // upstream fetches, all clients together
};

/** Ask the limiter for `key`. A limiter fault allows the request: it must not take the server down. */
export async function take(env, key, rules) {
  if (!env.LIMITER) return { ok: true };
  try {
    return await env.LIMITER.get(env.LIMITER.idFromName(key)).take(rules);
  } catch (e) {
    return { ok: true };
  }
}
