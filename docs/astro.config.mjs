// @ts-check
import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";
import rehypeMermaid from "rehype-mermaid";

// https://astro.build/config
export default defineConfig({
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
      description: "WASI HTTP runtime with JavaScript orchestration",
      social: [
        {
          icon: "github",
          label: "GitHub",
          href: "https://github.com/hlop3z/mik",
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
            { label: "Scripts & Orchestration", slug: "guides/scripts" },
            { label: "Reliability Features", slug: "guides/reliability" },
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
        baseUrl: "https://github.com/dufeut/mik/edit/main/docs/",
      },
      lastUpdated: true,
    }),
  ],
});
