import type { Metadata } from "next";
import { Geist, Geist_Mono } from "next/font/google";
import "./globals.css";

const geistSans = Geist({
  variable: "--font-geist-sans",
  subsets: ["latin"],
});

const geistMono = Geist_Mono({
  variable: "--font-geist-mono",
  subsets: ["latin"],
});

export const metadata: Metadata = {
  title: "Spool",
  description: "An open-source local design agent powered by small language models.",
};

// Runs while the HTML is parsed, before the first paint, so a visitor who
// prefers the dark theme never sees a flash of light paper. Same key as
// THEME_STORAGE_KEY in components/use-theme.ts.
const applyThemeBeforePaint = `(function(){var t=null;try{t=localStorage.getItem("spool-theme")}catch(e){}if(t!=="dark"&&t!=="light")t=matchMedia("(prefers-color-scheme: dark)").matches?"dark":"light";document.documentElement.setAttribute("data-theme",t)})()`;

export default function RootLayout({ children }: LayoutProps<"/">) {
  return (
    <html
      lang="en"
      className={`${geistSans.variable} ${geistMono.variable}`}
      data-theme="light"
      suppressHydrationWarning
    >
      <head>
        <script dangerouslySetInnerHTML={{ __html: applyThemeBeforePaint }} />
      </head>
      <body>{children}</body>
    </html>
  );
}
