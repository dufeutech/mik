// @ts-check
import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";
import rehypeMermaid from "rehype-mermaid";

// https://astro.build/config
export default defineConfig({
  site: "https://dufeutech.github.io",
  base: "/mik",
  markdown: {
    rehypePlugins: [
      [
        rehypeMermaid,
        {
          strategy: "img-svg", // Embed as <img> to avoid style conflicts
          dark: true, // Generate light/dark variants with <picture>
        },
      ],
    ],
    // Exclude mermaid from syntax highlighting (Astro 5.5+)
    syntaxHighlight: {
      excludeLangs: ["mermaid"],
    },
  },
  integrations: [
    starlight({
      title: "mik",
      description: "Package Manager and Runtime for WASI HTTP Components",
      social: [
        {
          icon: "github",
          label: "GitHub",
          href: "https://github.com/dufeutech/mik",
        },
      ],
      logo: {
        src: "./src/assets/logo.png",
        replacesTitle: false,
      },
      customCss: ["./src/styles/custom.css"],
      sidebar: [
        { label: "Introduction", slug: "index" },
        { label: "Getting Started", slug: "getting-started" },
        {
          label: "Guides",
          items: [
            { label: "Configuration", slug: "guides/configuration" },
            { label: "Building Components", slug: "guides/building-components" },
            { label: "Docker", slug: "guides/docker" },
            { label: "Daemon Services", slug: "guides/daemon" },
            { label: "Scripts & Orchestration", slug: "guides/scripts" },
            { label: "Reliability Features", slug: "guides/reliability" },
            { label: "Production Deployment", slug: "guides/production" },
            { label: "Monitoring", slug: "guides/monitoring" },
            { label: "Runbook", slug: "guides/runbook" },
            { label: "systemd Setup", slug: "guides/systemd" },
          ],
        },
        {
          label: "Reference",
          items: [
            { label: "CLI Reference", slug: "reference/cli" },
            { label: "Architecture", slug: "reference/architecture" },
            { label: "Sidecars", slug: "reference/sidecars" },
            { label: "Security", slug: "reference/security" },
          ],
        },
      ],
      pagefind: true,
      editLink: {
        baseUrl: "https://github.com/dufeutech/mik/edit/main/docs/",
      },
      lastUpdated: true,
    }),
  ],
});
