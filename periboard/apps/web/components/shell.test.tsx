import { expect, test } from "bun:test"
import { createElement, type FunctionComponent } from "react"
import { renderToStaticMarkup } from "react-dom/server"

import * as shell from "./shell"

test("the header clock has a stable accessible pre-hydration state", () => {
  const IstClock = Reflect.get(shell, "IstClock")

  expect(typeof IstClock).toBe("function")
  if (typeof IstClock !== "function") return

  const html = renderToStaticMarkup(
    createElement(IstClock as FunctionComponent)
  )
  expect(html).toContain("Current time in India Standard Time")
  expect(html).toContain("--:--:-- IST")
})
