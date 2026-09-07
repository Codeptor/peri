import { describe, expect, test } from "bun:test"

import * as format from "./format"

const FIXED_MS = Date.parse("2026-08-27T18:17:02.000Z")
const FIXED_SECONDS = FIXED_MS / 1000

describe("IST timestamps", () => {
  test("formats an exact decision timestamp in India Standard Time", () => {
    expect(format.stamp(FIXED_SECONDS)).toBe("27 Aug 2026 · 23:47:02 IST")
    expect(format.stamp(null)).toBe("—")
  })

  test("formats the compact live header clock in India Standard Time", () => {
    const istTime = Reflect.get(format, "istTime")

    expect(typeof istTime).toBe("function")
    if (typeof istTime === "function") {
      expect(istTime(FIXED_MS)).toBe("23:47:02 IST")
    }
  })
})
