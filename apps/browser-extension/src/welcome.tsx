import { render } from "solid-js/web";
import { connect } from "./ui/client";
import { initialLanguage } from "./ui/i18n";
import { Welcome } from "./ui/Welcome";
import "./ui/welcome.css";

void initialLanguage().finally(() => {
  connect();
  render(() => <Welcome />, document.getElementById("root")!);
});
