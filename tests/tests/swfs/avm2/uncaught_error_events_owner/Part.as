package {
    import flash.display.MovieClip;

    public class Part extends MovieClip {
        public static var throwNext:Boolean = false;

        public function Part() {
            addFrameScript(0, frame1);
        }

        private function frame1():void {
            if (throwNext) {
                throwNext = false;
                throw new Error("frame script");
            }
        }
    }
}
