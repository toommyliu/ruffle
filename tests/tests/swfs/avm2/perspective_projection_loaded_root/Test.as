package {
    import flash.display.Loader;
    import flash.display.MovieClip;
    import flash.display.Sprite;
    import flash.events.Event;
    import flash.external.ExternalInterface;
    import flash.geom.PerspectiveProjection;
    import flash.net.URLRequest;

    [SWF(width="550", height="400")]
    public class Test extends MovieClip {
        private var loader:Loader = new Loader();

        public function Test() {
            log("main root", root.transform.perspectiveProjection);
            loader.contentLoaderInfo.addEventListener(Event.COMPLETE, onLoad);
            loader.load(new URLRequest("child.swf"));
        }

        private function log(what:String, pp:PerspectiveProjection):void {
            var line:String = what + ": " + (pp == null ? "null"
                : "fieldOfView=" + pp.fieldOfView.toFixed(4) + " projectionCenter=" + pp.projectionCenter);
            trace(line);
            if (ExternalInterface.available) {
                ExternalInterface.call("report", line);
            }
        }

        private function onLoad(e:Event):void {
            var child:MovieClip = MovieClip(loader.content);
            log("loaded root in its Loader", child.transform.perspectiveProjection);
            addChild(loader);
            log("loaded root under the main root", child.transform.perspectiveProjection);
            stage.addChild(child);
            log("loaded root on the stage", child.transform.perspectiveProjection);
            var item:Sprite = new Sprite();
            child.addChild(item);
            log("root of an object under it (root is loaded root: " + (item.root == child) + ")", item.root.transform.perspectiveProjection);
            child.transform.perspectiveProjection = null;
            log("loaded root after setting null", child.transform.perspectiveProjection);
            stage.removeChild(child);
            log("loaded root off the display list", child.transform.perspectiveProjection);
            if (ExternalInterface.available) {
                ExternalInterface.call("report", "done");
            }
        }
    }
}
