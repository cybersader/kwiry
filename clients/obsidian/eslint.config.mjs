import { defineConfig, globalIgnores } from "eslint/config";
import obsidianmd from "eslint-plugin-obsidianmd";

export default defineConfig([
  globalIgnores(["**/node_modules/**", "main.js", ".tmp/**", "d5c-brat.tmp/**", "rust/**/pkg/**", "rust/**/target/**"]),
  {
    files: ["src/**/*.ts", "package.json", "manifest.json"],
    extends: obsidianmd.configs.recommended,
  },
  {
    files: ["src/**/*.ts"],
    languageOptions: {
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
  },
]);
