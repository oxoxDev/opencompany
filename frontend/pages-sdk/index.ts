// `@opencompany/site` — the small, curated set of the console's own site
// components an agent-authored dashboard page builds with, plus the
// postMessage `client` it reaches live company data through. See
// docs/spec/runtime/pages.md and the plan's §3/§6.
//
// A deliberately small subset of `frontend/src/components/ui/*`: the handful
// of primitives that compose almost everything else in the console
// (buttons, cards, inputs, labels, badges, separators) rather than an
// attempt to port the whole library. A page wanting a richer control can
// still compose these — the point is that whatever it builds carries the
// console's own visual language, not that every internal component ships.
import "./index.css";

export { Button, buttonVariants } from "@/components/ui/button";
export { Badge, badgeVariants } from "@/components/ui/badge";
export { Input } from "@/components/ui/input";
export { Label } from "@/components/ui/label";
export { Separator } from "@/components/ui/separator";
export {
  Card,
  CardHeader,
  CardTitle,
  CardDescription,
  CardAction,
  CardContent,
  CardFooter,
} from "@/components/ui/card";

export { client, type GraphQLResult } from "./client";
