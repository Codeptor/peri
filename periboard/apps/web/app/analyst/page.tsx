import type { Metadata } from "next"

import { AnalystChat } from "@/components/analyst/analyst-chat"

export const metadata: Metadata = {
  title: "Live analyst · periboard",
  description:
    "Grounded Qwen conversation and explicitly authorized trade actions.",
}

export default function AnalystPage() {
  return <AnalystChat />
}
