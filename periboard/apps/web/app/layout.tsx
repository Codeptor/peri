import { Geist, JetBrains_Mono, Merriweather } from "next/font/google"

import "@workspace/ui/globals.css"
import "streamdown/styles.css"
import { ThemeProvider } from "@/components/theme-provider"
import { Shell } from "@/components/shell"
import { LiveDashboardProvider } from "@/lib/use-live-dashboard"
import { cn } from "@workspace/ui/lib/utils"

const merriweatherHeading = Merriweather({
  subsets: ["latin"],
  variable: "--font-heading",
})

const fontSans = Geist({
  subsets: ["latin"],
  variable: "--font-sans",
})

const jetbrainsMono = JetBrains_Mono({
  subsets: ["latin"],
  variable: "--font-mono",
})

export const metadata = {
  title: "periboard",
  description: "peri — autonomous LLM perp trader, glass-box dashboard",
}

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode
}>) {
  return (
    <html
      lang="en"
      suppressHydrationWarning
      className={cn(
        "antialiased",
        fontSans.variable,
        jetbrainsMono.variable,
        merriweatherHeading.variable
      )}
    >
      <body>
        <ThemeProvider defaultTheme="dark">
          <LiveDashboardProvider>
            <Shell>{children}</Shell>
          </LiveDashboardProvider>
        </ThemeProvider>
      </body>
    </html>
  )
}
