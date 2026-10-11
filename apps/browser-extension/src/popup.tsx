import { render } from "solid-js/web";
import { connect } from "./ui/client";
import { initialLanguage } from "./ui/i18n";
import { Popup } from "./ui/Popup";
import "./ui/popup.css";

void initialLanguage().finally(() => {
  connect();
  render(() => <Popup />, document.getElementById("root")!);
});
