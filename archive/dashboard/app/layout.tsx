import type { Metadata } from "next"
import { Geist, Geist_Mono } from "next/font/google"

import "./globals.css"
import { cn } from "@/lib/utils"
import { AppSidebar, LiveStatus, MobileNav } from "@/components/app-sidebar"
import { StatusPill } from "@/components/blocks/status-pill"
import { CommandPalette } from "@/components/command-palette"
import { ThemeProvider } from "@/components/theme-provider"

const fontSans = Geist({
  subsets: ["latin"],
  variable: "--font-sans",
})

const fontMono = Geist_Mono({
  subsets: ["latin"],
  variable: "--font-mono",
})

export const metadata: Metadata = {
  title: "kestrel — paper desk",
  description: "Autonomous paper trader terminal (paper-only, no live orders).",
}

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode
}>) {
  return (
    <html
      className={cn(fontSans.variable, fontMono.variable)}
      lang="en"
      suppressHydrationWarning
    >
      <body className="antialiased">
        <ThemeProvider>
          <div className="flex min-h-svh">
            <AppSidebar />
            <div className="flex min-w-0 flex-1 flex-col">
              <header className="flex h-12 shrink-0 items-center gap-3 px-5 md:px-8">
                <span className="mr-auto flex items-center gap-2 md:hidden">
                  <span className="text-sm font-semibold tracking-tight">
                    Kestrel
                  </span>
                  <StatusPill tone="accent">Paper</StatusPill>
                </span>
                <div className="ml-auto">
                  <LiveStatus />
                </div>
              </header>
              <main className="mx-auto w-full max-w-[1600px] flex-1 px-5 pb-24 md:px-8 md:pb-10">
                {children}
              </main>
            </div>
          </div>
          <MobileNav />
          <CommandPalette />
        </ThemeProvider>
      </body>
    </html>
  )
}
